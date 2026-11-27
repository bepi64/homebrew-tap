use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use reqwest::redirect::Policy;
use reqwest::Url;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router, ErrorData,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::{DiskCache, DEFAULT_CACHE_TTL, PINNED_CACHE_TTL, SEARCH_CACHE_TTL};
use crate::llms_txt::{build_llms_txt_url, fetch_llms_with_safe_redirects};
use crate::local_docs::search_local_docs;
use crate::manifest::{detect_project_ecosystem, resolve_go_short_name, resolve_project_version};

/// Minimum configurable output budget for `fetch_docs`.
pub const MIN_OUTPUT_CHARS: usize = 500;

/// Default maximum formatted output returned by a single `fetch_docs` tool call.
pub const MAX_OUTPUT_CHARS: usize = 6500;

/// Upper bound on caller-requested `max_chars` to keep MCP tool responses bounded.
pub const MAX_ALLOWED_OUTPUT_CHARS: usize = 20_000;

/// Clamp an optional caller-supplied output budget into `[MIN_OUTPUT_CHARS, MAX_ALLOWED_OUTPUT_CHARS]`.
#[must_use]
pub fn clamp_max_chars(requested: Option<usize>) -> usize {
    requested.map_or(MAX_OUTPUT_CHARS, |limit| {
        limit.clamp(MIN_OUTPUT_CHARS, MAX_ALLOWED_OUTPUT_CHARS)
    })
}

/// Maximum uncompressed HTTP response body size accepted from upstream servers (5 MiB).
pub const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

/// Total HTTP request timeout (aligned with Nielsen's 10-second interactive ceiling).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// TCP and TLS connection establishment timeout.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum number of same-allowlist HTTPS redirects followed per request.
const MAX_REDIRECTS: usize = 5;

/// Maximum accepted character length for library or version parameters.
const MAX_IDENTIFIER_LEN: usize = 200;

/// Allowlisted upstream hosts permitted for outbound HTTPS requests.
pub const ALLOWED_HOSTS: [&str; 6] = [
    "docs.rs",
    "pypi.org",
    "pkg.go.dev",
    "api.github.com",
    "registry.npmjs.org",
    "unpkg.com",
];

const USER_AGENT: &str = concat!(
    "fast-docs/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/bepi64/homebrew-tap)"
);

/// Supported documentation ecosystems.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecosystem {
    Rust,
    Python,
    Go,
    Npm,
}

impl Ecosystem {
    /// Parse an optional user-provided language identifier.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "rust" | "rs" => Some(Self::Rust),
            "python" | "py" => Some(Self::Python),
            "go" | "golang" => Some(Self::Go),
            "npm" | "js" | "javascript" | "ts" | "typescript" | "node" => Some(Self::Npm),
            _ => None,
        }
    }

    /// Canonical lowercase label used in Context7 output blocks.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::Go => "go",
            Self::Npm => "npm",
        }
    }
}

/// Parse an optional `lang` argument strictly, returning `Err` on unknown/misspelled values
/// instead of silently falling back to auto-detection.
///
/// # Errors
/// Returns an error listing valid ecosystems when `raw` is non-empty and unrecognized.
pub fn parse_ecosystem_arg(raw: Option<&str>) -> Result<Option<Ecosystem>, String> {
    let Some(value) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    Ecosystem::parse(value).map(Some).ok_or_else(|| {
        format!(
            "invalid lang '{value}': expected one of rust, python, go, or npm (aliases: rs, py, golang, js, ts, node)"
        )
    })
}

/// Scope selector for `search_docs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchScope {
    All,
    Only(Ecosystem),
}

impl SearchScope {
    fn parse_arg(raw: Option<&str>) -> Result<Self, String> {
        let Some(value) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(Self::All);
        };
        if value.eq_ignore_ascii_case("all") {
            return Ok(Self::All);
        }
        Ecosystem::parse(value).map(Self::Only).ok_or_else(|| {
            format!("invalid lang '{value}': expected one of rust, python, go, npm, or all")
        })
    }

    const fn as_key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Only(eco) => eco.as_str(),
        }
    }

    const fn includes(self, ecosystem: Ecosystem) -> bool {
        match self {
            Self::All => true,
            Self::Only(target) => matches!(
                (target, ecosystem),
                (Ecosystem::Rust, Ecosystem::Rust)
                    | (Ecosystem::Python, Ecosystem::Python)
                    | (Ecosystem::Go, Ecosystem::Go)
                    | (Ecosystem::Npm, Ecosystem::Npm)
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FetchDocsRequest {
    #[schemars(
        description = "Library name or module path (e.g. tokio, flask, gin, zod, @modelcontextprotocol/sdk, github.com/gin-gonic/gin)"
    )]
    pub library: String,
    #[schemars(description = "Optional version (default: latest)")]
    pub version: Option<String>,
    #[schemars(
        description = "Language ecosystem: rust, python, go, or npm/js/ts (default: auto-detect)"
    )]
    pub lang: Option<String>,
    #[schemars(
        description = "Optional submodule or item path (e.g. sync, sync::mpsc, sync/struct.RwLock.html for Rust; binding for Go)"
    )]
    pub module: Option<String>,
    #[schemars(
        description = "Optional topic keywords used to rank and filter relevant documentation sections (e.g. 'rwlock mutex', 'routing blueprint')"
    )]
    pub topic: Option<String>,
    #[schemars(
        description = "Optional output character budget (default: 6500, clamped to 500..=20000)"
    )]
    pub max_chars: Option<usize>,
    #[schemars(
        description = "Optional local project directory path used to auto-resolve the library version from Cargo.lock, Cargo.toml, package-lock.json, package.json, uv.lock, pyproject.toml, or go.mod when version is omitted"
    )]
    pub project_dir: Option<String>,
    #[schemars(
        description = "Optional byte offset for paginating long documentation responses (default: 0)"
    )]
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchDocsRequest {
    #[schemars(description = "Search query for documentation")]
    pub query: String,
    #[schemars(
        description = "Language ecosystem to search: rust, python, go, npm, or all (default: all)"
    )]
    pub lang: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FetchLlmsTxtRequest {
    #[schemars(
        description = "Documentation site domain or HTTPS URL (e.g. hono.dev, docs.pydantic.dev, https://vite.dev/llms.txt)"
    )]
    pub target: String,
    #[schemars(
        description = "When true, fetch /llms-full.txt with automatic fallback to /llms.txt on HTTP 404 (default: false)"
    )]
    pub full: Option<bool>,
    #[schemars(
        description = "Optional topic keywords used to rank and filter relevant sections of the llms.txt document"
    )]
    pub topic: Option<String>,
    #[schemars(
        description = "Optional output character budget (default: 6500, clamped to 500..=20000)"
    )]
    pub max_chars: Option<usize>,
    #[schemars(
        description = "Optional byte offset for paginating long llms.txt responses (default: 0)"
    )]
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FetchLocalDocsRequest {
    #[schemars(
        description = "Local project directory containing Markdown/text documentation files (e.g. README.md, docs/*.md)"
    )]
    pub project_dir: String,
    #[schemars(
        description = "Optional topic keywords used to rank and filter relevant local documentation sections"
    )]
    pub topic: Option<String>,
    #[schemars(
        description = "Optional output character budget (default: 6500, clamped to 500..=20000)"
    )]
    pub max_chars: Option<usize>,
    #[schemars(
        description = "Optional byte offset for paginating long local documentation results (default: 0)"
    )]
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ManageCacheRequest {
    #[schemars(
        description = "Cache management action: 'stats' (default), 'prune' (remove expired/corrupt entries), or 'clear' (remove all entries)"
    )]
    pub action: Option<String>,
}

fn validate_npm_segment(segment: &str) -> Result<(), String> {
    if segment.is_empty() || segment == "." || segment.contains("..") {
        return Err("invalid npm package name: empty or traversal segment".to_string());
    }
    for ch in segment.chars() {
        let allowed = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.');
        if !allowed {
            return Err(format!(
                "invalid npm package name: forbidden character '{ch}'"
            ));
        }
    }
    Ok(())
}

/// Validate a library or module identifier before interpolating it into an upstream URL.
fn validate_library_name(library: &str, ecosystem: Ecosystem) -> Result<&str, String> {
    if library.is_empty() || library.len() > MAX_IDENTIFIER_LEN {
        return Err("invalid library name: empty or exceeds maximum length".to_string());
    }
    if library.starts_with('/') || library.ends_with('/') || library.contains("//") {
        return Err("invalid library name: malformed path separator".to_string());
    }
    if library.contains("..") || library.contains("://") {
        return Err("invalid library name: path traversal or scheme is forbidden".to_string());
    }

    if ecosystem == Ecosystem::Npm {
        if let Some(rest) = library.strip_prefix('@') {
            let mut parts = rest.split('/');
            let (Some(scope), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
                return Err(
                    "invalid scoped npm package name: expected '@scope/package'".to_string()
                );
            };
            validate_npm_segment(scope)?;
            validate_npm_segment(name)?;
            return Ok(library);
        }
        validate_npm_segment(library)?;
        return Ok(library);
    }

    let allow_slash = matches!(ecosystem, Ecosystem::Go);
    for ch in library.chars() {
        let allowed = ch.is_ascii_alphanumeric()
            || matches!(ch, '-' | '_' | '.')
            || (allow_slash && ch == '/');
        if !allowed {
            return Err(format!("invalid library name: forbidden character '{ch}'"));
        }
    }

    Ok(library)
}

/// Validate a package version string before interpolating it into an upstream URL.
fn validate_version(version: &str) -> Result<&str, String> {
    if version.is_empty() {
        return Ok("latest");
    }
    if version.len() > MAX_IDENTIFIER_LEN {
        return Err("invalid version: exceeds maximum length".to_string());
    }
    if version.contains("..") {
        return Err("invalid version: path traversal is forbidden".to_string());
    }
    for ch in version.chars() {
        let allowed = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '+');
        if !allowed {
            return Err(format!("invalid version: forbidden character '{ch}'"));
        }
    }
    Ok(version)
}

/// Verify that an outbound URL uses HTTPS on an allowlisted host with no embedded credentials or
/// non-default port.
///
/// # Errors
/// Returns an error description if the URL cannot be parsed or violates the outbound allowlist.
pub fn validate_outbound_url(raw_url: &str) -> Result<Url, String> {
    let parsed = Url::parse(raw_url).map_err(|err| format!("invalid URL: {err}"))?;
    validate_parsed_url(&parsed)?;
    Ok(parsed)
}

fn validate_parsed_url(parsed: &Url) -> Result<(), String> {
    if parsed.scheme() != "https" {
        return Err(format!("forbidden URL scheme '{}'", parsed.scheme()));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("URLs with embedded credentials are forbidden".to_string());
    }
    if parsed.port().is_some() {
        return Err("URLs with explicit ports are forbidden".to_string());
    }
    let Some(host) = parsed.host_str() else {
        return Err("URL is missing a host".to_string());
    };
    if !ALLOWED_HOSTS.contains(&host) {
        return Err(format!("host '{host}' is not in the outbound allowlist"));
    }
    Ok(())
}

fn looks_like_go_import_path(lower: &str) -> bool {
    if lower.starts_with('@') {
        return false;
    }
    if let Some((first_seg, _rest)) = lower.split_once('/') {
        // Vanity domain import (e.g. github.com/..., go.yaml.in/..., gopkg.in/..., k8s.io/...)
        if first_seg.contains('.') && !first_seg.starts_with('.') && !first_seg.ends_with('.') {
            return true;
        }
        // Standard Go stdlib multi-segment packages (net/http, encoding/json, crypto/tls, ...)
        let go_stdlib_roots = [
            "archive",
            "bufio",
            "compress",
            "container",
            "crypto",
            "database",
            "debug",
            "encoding",
            "go",
            "hash",
            "html",
            "image",
            "index",
            "io",
            "log",
            "math",
            "mime",
            "net",
            "os",
            "path",
            "plugin",
            "reflect",
            "regexp",
            "runtime",
            "sync",
            "syscall",
            "testing",
            "text",
            "time",
            "unicode",
        ];
        if go_stdlib_roots.contains(&first_seg) {
            return true;
        }
    }
    let go_stdlib_single = [
        "fmt", "strings", "bytes", "context", "errors", "strconv", "sort", "slices", "maps",
        "filepath",
    ];
    go_stdlib_single.contains(&lower)
}

/// Detect the most likely documentation ecosystem for a library identifier.
#[must_use]
pub fn detect_lang(library: &str) -> Ecosystem {
    let lower = library.to_ascii_lowercase();
    if looks_like_go_import_path(&lower) {
        return Ecosystem::Go;
    }

    let python_libs = [
        "flask",
        "django",
        "fastapi",
        "requests",
        "numpy",
        "pandas",
        "pytest",
        "pydantic",
        "sqlalchemy",
        "celery",
        "boto3",
        "pip",
        "httpx",
        "aiohttp",
        "uvicorn",
        "rich",
        "typer",
        "click",
    ];
    let go_libs = [
        "gin", "echo", "fiber", "cobra", "viper", "gorm", "zap", "logrus", "chi", "mux", "gorilla",
        "fx",
    ];
    let npm_libs = [
        "react",
        "react-dom",
        "next",
        "vue",
        "nuxt",
        "svelte",
        "angular",
        "express",
        "fastify",
        "hono",
        "zod",
        "typescript",
        "vite",
        "vitest",
        "eslint",
        "prettier",
        "tailwindcss",
        "axios",
        "lodash",
        "prisma",
        "drizzle-orm",
        "webpack",
        "esbuild",
        "playwright",
        "jest",
        "trpc",
        "rxjs",
        "chalk",
        "commander",
    ];

    if lower.starts_with('@') && lower.contains('/') {
        return Ecosystem::Npm;
    }
    if python_libs.contains(&lower.as_str()) {
        return Ecosystem::Python;
    }
    if go_libs.contains(&lower.as_str()) {
        return Ecosystem::Go;
    }
    if npm_libs.contains(&lower.as_str()) {
        return Ecosystem::Npm;
    }
    Ecosystem::Rust
}

/// Map common short Go package aliases to their canonical Go module paths.
#[must_use]
pub fn resolve_go_module(library: &str) -> String {
    match library.to_ascii_lowercase().as_str() {
        "gin" => "github.com/gin-gonic/gin".to_string(),
        "echo" => "github.com/labstack/echo/v4".to_string(),
        "fiber" => "github.com/gofiber/fiber/v2".to_string(),
        "cobra" => "github.com/spf13/cobra".to_string(),
        "viper" => "github.com/spf13/viper".to_string(),
        "gorm" => "gorm.io/gorm".to_string(),
        "zap" => "go.uber.org/zap".to_string(),
        "logrus" => "github.com/sirupsen/logrus".to_string(),
        "chi" => "github.com/go-chi/chi/v5".to_string(),
        "mux" | "gorilla" => "github.com/gorilla/mux".to_string(),
        _ => library.to_string(),
    }
}

/// Validate and normalize an optional submodule or API item path.
fn validate_module_path(raw: &str, ecosystem: Ecosystem) -> Result<String, String> {
    if raw.is_empty() || raw.len() > MAX_IDENTIFIER_LEN {
        return Err("invalid module path: empty or exceeds maximum length".to_string());
    }
    if raw.contains("://") || raw.contains(":::") || raw.contains("..") {
        return Err("invalid module path: traversal or scheme is forbidden".to_string());
    }

    let normalized = match ecosystem {
        Ecosystem::Rust => raw.replace("::", "/"),
        Ecosystem::Go | Ecosystem::Python | Ecosystem::Npm => raw.to_string(),
    };

    if normalized.starts_with('/') || normalized.ends_with('/') || normalized.contains("//") {
        return Err("invalid module path: malformed path separator".to_string());
    }

    for ch in normalized.chars() {
        let allowed = ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/');
        if !allowed {
            return Err(format!("invalid module path: forbidden character '{ch}'"));
        }
    }

    Ok(normalized)
}

/// Extract the latest release version from a `PyPI` RSS feed
/// (`https://pypi.org/rss/project/<pkg>/releases.xml`), which is ~12 KiB even for packages whose
/// full `/pypi/<pkg>/json` payload exceeds the 5 MiB response cap.
#[must_use]
pub fn extract_latest_version_from_pypi_rss(rss: &str) -> Option<String> {
    let after_item = rss.split_once("<item>")?.1;
    let after_title = after_item.split_once("<title>")?.1;
    let title = after_title.split_once("</title>")?.0.trim();
    validate_version(title).ok().map(ToOwned::to_owned)
}

/// Build candidate `docs.rs` item URLs (`struct.`, `trait.`, `enum.`) when a Rust `module` path
/// ends with an `UpperCamelCase` item identifier (such as `builder::Command` or `ser::Serialize`).
#[must_use]
pub fn rustdoc_item_fallback_urls(library: &str, version: &str, module: &str) -> Vec<String> {
    let Ok(library) = validate_library_name(library, Ecosystem::Rust) else {
        return Vec::new();
    };
    let Ok(version) = validate_version(version) else {
        return Vec::new();
    };
    let Ok(normalized) = validate_module_path(module.trim(), Ecosystem::Rust) else {
        return Vec::new();
    };
    if Path::new(&normalized)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("html"))
    {
        return Vec::new();
    }

    let (parent, item) = match normalized.rsplit_once('/') {
        Some((p, i)) => (Some(p), i),
        None => (None, normalized.as_str()),
    };
    if !item.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        return Vec::new();
    }

    let crate_ident = library.replace('-', "_");
    let mut urls = Vec::with_capacity(3);
    for kind in ["struct", "trait", "enum"] {
        let candidate = match parent {
            Some(p) => {
                format!("https://docs.rs/{library}/{version}/{crate_ident}/{p}/{kind}.{item}.html")
            }
            None => format!("https://docs.rs/{library}/{version}/{crate_ident}/{kind}.{item}.html"),
        };
        if validate_outbound_url(&candidate).is_ok() {
            urls.push(candidate);
        }
    }
    urls
}

/// Split a `docs.rs` search release label such as `"globset-0.4.22"` into `("globset", Some("0.4.22"))`.
#[must_use]
pub fn split_rust_release_name(raw: &str) -> (&str, Option<&str>) {
    let trimmed = raw.trim();
    for (idx, _) in trimmed.match_indices('-') {
        let after = &trimmed[idx + 1..];
        if after.chars().next().is_some_and(|c| c.is_ascii_digit())
            && after.contains('.')
            && idx > 0
        {
            return (&trimmed[..idx], Some(after));
        }
    }
    (trimmed, None)
}

/// Extract a valid `PyPI` exact-match package candidate from a single- or multi-word search query
/// (for example `"pytest fixtures"` → `Some("pytest")`).
#[must_use]
pub fn extract_pypi_candidate(raw_query: &str) -> Option<&str> {
    let trimmed = raw_query.trim();
    if trimmed.is_empty() {
        return None;
    }
    let first_word = trimmed.split_whitespace().next()?;
    validate_library_name(first_word, Ecosystem::Python).ok()
}

/// Build and validate an allowlisted upstream documentation URL.
///
/// # Errors
/// Returns an error if `library` or `version` contains forbidden characters or fails URL validation.
pub fn build_url(library: &str, version: &str, ecosystem: Ecosystem) -> Result<String, String> {
    build_url_with_module(library, version, ecosystem, None)
}

/// Build and validate an allowlisted upstream documentation URL, optionally targeting a submodule
/// or specific item page.
///
/// # Errors
/// Returns an error if `library`, `version`, or `module` contains forbidden characters or violates
/// the outbound allowlist.
pub fn build_url_with_module(
    library: &str,
    version: &str,
    ecosystem: Ecosystem,
    module: Option<&str>,
) -> Result<String, String> {
    let library = validate_library_name(library, ecosystem)?;
    let version = validate_version(version)?;
    let normalized_module = match module.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => Some(validate_module_path(raw, ecosystem)?),
        None => None,
    };

    let candidate = match ecosystem {
        Ecosystem::Python => {
            if version == "latest" {
                format!("https://pypi.org/pypi/{library}/json")
            } else {
                format!("https://pypi.org/pypi/{library}/{version}/json")
            }
        }
        Ecosystem::Go => {
            let go_mod = resolve_go_module(library);
            let base = if version == "latest" {
                format!("https://pkg.go.dev/{go_mod}")
            } else {
                format!("https://pkg.go.dev/{go_mod}@{version}")
            };
            match normalized_module {
                Some(sub) => format!("{base}/{sub}"),
                None => base,
            }
        }
        Ecosystem::Rust => match normalized_module {
            Some(sub) => {
                let crate_ident = library.replace('-', "_");
                if std::path::Path::new(&sub)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("html"))
                {
                    format!("https://docs.rs/{library}/{version}/{crate_ident}/{sub}")
                } else {
                    format!("https://docs.rs/{library}/{version}/{crate_ident}/{sub}/")
                }
            }
            None => format!("https://docs.rs/{library}/{version}"),
        },
        Ecosystem::Npm => format!("https://unpkg.com/{library}@{version}/README.md"),
    };

    validate_outbound_url(&candidate)?;
    Ok(candidate)
}

/// Percent-encode a query string component using `application/x-www-form-urlencoded` rules.
#[must_use]
pub fn urlencoding(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push('+'),
            other => {
                let _ = write!(encoded, "%{other:02X}");
            }
        }
    }
    encoded
}

/// Result of a conditional HTTP GET request (`If-None-Match`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    /// Upstream returned a `2xx` body, optional `ETag`, and final resolved URL after redirects.
    Fetched {
        body: String,
        etag: Option<String>,
        final_url: String,
    },
    /// Upstream returned `304 Not Modified`; cached body remains valid.
    NotModified { etag: Option<String> },
}

/// Fetch an HTTP response body while enforcing a total timeout, incremental byte cap
/// (applied to the decompressed stream), and optional RFC 9111 `If-None-Match` validation.
///
/// # Errors
/// Returns an error if the request fails, times out, returns a non-success HTTP status
/// (other than `304 Not Modified`), exceeds `max_bytes`, or contains invalid UTF-8.
pub async fn fetch_url_conditional(
    client: &reqwest::Client,
    url: &str,
    if_none_match: Option<&str>,
    timeout: Duration,
    max_bytes: usize,
) -> Result<FetchOutcome, String> {
    tokio::time::timeout(timeout, async {
        let mut req = client.get(url);
        if let Some(etag) = if_none_match {
            req = req.header(reqwest::header::IF_NONE_MATCH, etag);
        }

        let mut resp = req
            .send()
            .await
            .map_err(|err| format!("request failed: {err}"))?;

        let final_url = resp.url().to_string();
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|val| val.to_str().ok())
            .map(ToOwned::to_owned);

        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(FetchOutcome::NotModified { etag });
        }

        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status().as_u16()));
        }

        if let Some(content_len) = resp.content_length() {
            if usize::try_from(content_len).map_or(true, |len| len > max_bytes) {
                return Err(format!(
                    "response exceeds maximum allowed size of {max_bytes} bytes"
                ));
            }
        }

        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|err| format!("response stream error: {err}"))?
        {
            if buf.len().saturating_add(chunk.len()) > max_bytes {
                return Err(format!(
                    "response exceeds maximum allowed size of {max_bytes} bytes"
                ));
            }
            buf.extend_from_slice(&chunk);
        }

        let body =
            String::from_utf8(buf).map_err(|err| format!("response is not valid UTF-8: {err}"))?;
        Ok(FetchOutcome::Fetched {
            body,
            etag,
            final_url,
        })
    })
    .await
    .map_err(|_| "request timeout".to_string())?
}

/// Fetch an HTTP response body while enforcing both a total timeout and an incremental byte cap.
///
/// # Errors
/// Returns an error if the request fails, times out, returns a non-success HTTP status, exceeds
/// `max_bytes`, or contains invalid UTF-8.
pub async fn fetch_url_with_limits(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
    max_bytes: usize,
) -> Result<String, String> {
    match fetch_url_conditional(client, url, None, timeout, max_bytes).await? {
        FetchOutcome::Fetched { body, .. } => Ok(body),
        FetchOutcome::NotModified { .. } => Err("unexpected HTTP 304 Not Modified".to_string()),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocSection {
    pub title: String,
    pub descriptions: Vec<String>,
    pub codes: Vec<String>,
    #[serde(default)]
    pub code_langs: Vec<Option<String>>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParsedDoc {
    sections: Vec<DocSection>,
    fallback: String,
    source_url: String,
    lang: String,
    #[serde(default)]
    metadata: Vec<String>,
}

impl ParsedDoc {
    fn render(&self, topic: Option<&str>, max_chars: usize) -> String {
        self.render_paginated(topic, max_chars, 0)
    }

    fn render_paginated(&self, topic: Option<&str>, max_chars: usize, offset: usize) -> String {
        let prefix = if self.metadata.is_empty() {
            String::new()
        } else {
            format!("{}\n\n", self.metadata.join("\n"))
        };

        if self.sections.is_empty() {
            let combined = format!("{prefix}{}", self.fallback);
            return slice_with_pagination(&combined, offset, max_chars);
        }

        let body = format_sections_all(
            &self.sections,
            &self.source_url,
            &self.lang,
            topic,
            256 * 1024,
        );
        let full = format!("{prefix}{body}");
        slice_with_pagination(&full, offset, max_chars)
    }
}

pub(crate) fn parse_html_sections(
    html: &str,
    source_label: &str,
    lib_name: &str,
) -> Vec<DocSection> {
    let parsed = parse_html_doc(html, source_label, Ecosystem::Rust, lib_name);
    let source_opt = (!source_label.is_empty()).then(|| source_label.to_string());
    if parsed.sections.is_empty() {
        let fallback = parsed.fallback.trim();
        if fallback.is_empty() {
            return Vec::new();
        }
        return vec![DocSection {
            title: format!("{lib_name} Overview"),
            descriptions: vec![fallback.to_string()],
            codes: Vec::new(),
            code_langs: Vec::new(),
            source: source_opt,
        }];
    }
    parsed
        .sections
        .into_iter()
        .map(|mut s| {
            s.source.clone_from(&source_opt);
            s
        })
        .collect()
}

fn clean_heading_text(raw: &str) -> String {
    strip_html_tags(raw)
        .trim_start_matches('§')
        .trim_end_matches(['§', '¶'])
        .replace('¶', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn clean_code_signature(raw: &str) -> String {
    raw.trim_start_matches('§')
        .trim_end_matches(['§', '¶'])
        .replace('¶', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_clean_code_block(pre: &scraper::ElementRef<'_>) -> String {
    // Rustdoc wraps code inside `<pre><code>` alongside optional `<span class="line-numbers">`.
    if let Ok(code_sel) = scraper::Selector::parse("code") {
        if let Some(code_el) = pre.select(&code_sel).next() {
            let txt = code_el.text().collect::<String>().trim().to_string();
            if !txt.is_empty() {
                return txt;
            }
        }
    }
    pre.text().collect::<String>().trim().to_string()
}

fn has_nested_list_items(li: &scraper::ElementRef<'_>) -> bool {
    scraper::Selector::parse("li").is_ok_and(|sel| li.select(&sel).next().is_some())
}

fn clean_html_fallback(doc: &scraper::Html) -> String {
    let mut parts = Vec::new();
    let root = doc.root_element();
    if let Ok(body_sel) = scraper::Selector::parse("body") {
        let scope = doc.select(&body_sel).next().unwrap_or(root);
        for desc in scope.descendants() {
            if let Some(txt) = desc.value().as_text() {
                let in_ignored = desc.ancestors().any(|anc| {
                    anc.value().as_element().is_some_and(|el| {
                        matches!(el.name(), "script" | "style" | "noscript" | "template")
                    })
                });
                if in_ignored {
                    continue;
                }
                let cleaned = txt.split_whitespace().collect::<Vec<_>>().join(" ");
                if !cleaned.is_empty() {
                    parts.push(cleaned);
                }
            }
        }
    }
    parts.join(" ")
}

fn parse_container_elements(
    container: &scraper::ElementRef<'_>,
    children_selector: &scraper::Selector,
    default_title: &str,
    sections: &mut Vec<DocSection>,
) {
    let mut cur_title = default_title.to_string();
    let mut cur_desc: Vec<String> = Vec::new();
    let mut cur_code: Vec<String> = Vec::new();
    let mut cur_code_langs: Vec<Option<String>> = Vec::new();

    for el in container.select(children_selector) {
        match el.value().name() {
            "h1" | "h2" | "h3" | "h4" => {
                if !cur_desc.is_empty() || !cur_code.is_empty() {
                    sections.push(DocSection {
                        title: cur_title.clone(),
                        descriptions: std::mem::take(&mut cur_desc),
                        codes: std::mem::take(&mut cur_code),
                        code_langs: std::mem::take(&mut cur_code_langs),
                        source: None,
                    });
                }
                let heading = clean_heading_text(&clean_inline_text(&el));
                if !heading.is_empty() {
                    cur_title = heading;
                }
            }
            "pre" => {
                let code = extract_clean_code_block(&el);
                if !code.is_empty() {
                    cur_code.push(code);
                    cur_code_langs.push(None);
                }
            }
            "p" => {
                let txt = clean_inline_text(&el);
                if txt.len() > 15 && !cur_desc.contains(&txt) {
                    cur_desc.push(txt);
                }
            }
            "li" => {
                // Skip outer <li> wrappers that contain nested <li> elements to prevent duplication.
                if !has_nested_list_items(&el) {
                    let txt = clean_inline_text(&el);
                    if txt.len() > 15 && !cur_desc.contains(&txt) {
                        cur_desc.push(txt);
                    }
                }
            }
            _ => {}
        }
    }

    if !cur_desc.is_empty() || !cur_code.is_empty() {
        sections.push(DocSection {
            title: cur_title,
            descriptions: cur_desc,
            codes: cur_code,
            code_langs: cur_code_langs,
            source: None,
        });
    }
}

fn extract_rustdoc_methods(doc: &scraper::Html, sections: &mut Vec<DocSection>) {
    let (Ok(method_sel), Ok(header_sel), Ok(docblock_sel), Ok(p_sel), Ok(pre_sel)) = (
        scraper::Selector::parse("section.method, details.toggle.method-toggle"),
        scraper::Selector::parse("h4.code-header, h3.code-header"),
        scraper::Selector::parse("div.docblock"),
        scraper::Selector::parse("p"),
        scraper::Selector::parse("pre"),
    ) else {
        return;
    };

    for method in doc.select(&method_sel) {
        let Some(header_el) = method.select(&header_sel).next() else {
            continue;
        };
        let sig = clean_code_signature(&clean_inline_text(&header_el));
        if sig.is_empty() {
            continue;
        }

        let mut descs = Vec::new();
        let mut codes = Vec::new();
        let mut code_langs = Vec::new();
        if let Some(docblock) = method.select(&docblock_sel).next() {
            for p in docblock.select(&p_sel) {
                let txt = clean_inline_text(&p);
                if !txt.is_empty() && !descs.contains(&txt) {
                    descs.push(txt);
                }
            }
            for pre in docblock.select(&pre_sel) {
                let code = extract_clean_code_block(&pre);
                if !code.is_empty() {
                    codes.push(code);
                    code_langs.push(Some("rust".to_string()));
                }
            }
        }
        if descs.is_empty() {
            descs.push(sig.clone());
        }
        sections.push(DocSection {
            title: sig,
            descriptions: descs,
            codes,
            code_langs,
            source: None,
        });
    }
}

fn parse_html_doc(html: &str, source_url: &str, ecosystem: Ecosystem, lib_name: &str) -> ParsedDoc {
    let doc = scraper::Html::parse_document(html);

    let Ok(children_selector) = scraper::Selector::parse("h1, h2, h3, h4, p, li, pre") else {
        return ParsedDoc {
            sections: Vec::new(),
            fallback: clean_html_fallback(&doc),
            source_url: source_url.to_string(),
            lang: ecosystem.as_str().to_string(),
            metadata: Vec::new(),
        };
    };

    let mut sections: Vec<DocSection> = Vec::new();
    let mut matched_container = false;

    // On pkg.go.dev, extract BOTH README and API Documentation sections instead of stopping at README.
    let go_containers = ["div.UnitReadme-content", "div.Documentation-content"];
    let mut matched_go = false;
    for sel_str in go_containers {
        if let Ok(sel) = scraper::Selector::parse(sel_str) {
            let containers: Vec<_> = doc.select(&sel).collect();
            if !containers.is_empty() {
                matched_go = true;
                matched_container = true;
                let default_title = if sel_str == "div.Documentation-content" {
                    format!("{lib_name} API Documentation")
                } else {
                    format!("{lib_name} Overview")
                };
                for container in containers {
                    parse_container_elements(
                        &container,
                        &children_selector,
                        &default_title,
                        &mut sections,
                    );
                }
            }
        }
    }

    if !matched_go {
        // On docs.rs, prefer the top-level crate/item docblock (not method docblocks extracted below).
        let primary_selectors = [
            "section#main-content > details.top-doc > div.docblock",
            "section#main-content > div.docblock",
            "div.docblock",
            "div.project-description",
            "article",
            "main",
        ];

        for sel_str in primary_selectors {
            let Ok(sel) = scraper::Selector::parse(sel_str) else {
                continue;
            };
            let containers: Vec<_> = doc.select(&sel).collect();
            if containers.is_empty() {
                continue;
            }
            matched_container = true;
            let default_title = format!("{lib_name} Overview");
            for container in containers {
                parse_container_elements(
                    &container,
                    &children_selector,
                    &default_title,
                    &mut sections,
                );
            }
            if !sections.is_empty() {
                break;
            }
        }
    }

    // Extract rustdoc method signatures (struct/trait pages) and item tables (crate/module pages).
    extract_rustdoc_methods(&doc, &mut sections);
    extract_rustdoc_item_tables(&doc, &mut sections);
    if !sections.is_empty() {
        matched_container = true;
    }

    let fallback = if !matched_container || sections.is_empty() {
        clean_html_fallback(&doc)
    } else {
        String::new()
    };

    ParsedDoc {
        sections: if matched_container {
            sections
        } else {
            Vec::new()
        },
        fallback,
        source_url: source_url.to_string(),
        lang: ecosystem.as_str().to_string(),
        metadata: Vec::new(),
    }
}

fn extract_rustdoc_item_tables(doc: &scraper::Html, sections: &mut Vec<DocSection>) {
    let (Ok(header_sel), Ok(a_sel)) = (
        scraper::Selector::parse("h2.section-header"),
        scraper::Selector::parse("a"),
    ) else {
        return;
    };

    for header in doc.select(&header_sel) {
        let heading = clean_inline_text(&header)
            .trim_end_matches('§')
            .trim()
            .to_string();
        if heading.is_empty() {
            continue;
        }

        // Find the next element sibling; rustdoc places `<dl class="item-table">` right after `<h2>`.
        let mut sibling = header.next_sibling();
        let mut table_el = None;
        while let Some(node) = sibling {
            if let Some(el) = scraper::ElementRef::wrap(node) {
                if el.value().name() == "dl"
                    && el
                        .value()
                        .attr("class")
                        .is_some_and(|c| c.split_whitespace().any(|cls| cls == "item-table"))
                {
                    table_el = Some(el);
                }
                break;
            }
            sibling = node.next_sibling();
        }

        let Some(dl) = table_el else {
            continue;
        };

        let mut items = Vec::new();
        if heading.eq_ignore_ascii_case("modules") {
            items.push(
                "Tip: pass module=\"<name>\" to fetch_docs to read any submodule below."
                    .to_string(),
            );
        }

        let mut pending_name: Option<String> = None;
        for child in dl.children() {
            let Some(el) = scraper::ElementRef::wrap(child) else {
                continue;
            };
            match el.value().name() {
                "dt" => {
                    if let Some(prev) = pending_name.take() {
                        items.push(prev);
                    }
                    let name = el
                        .select(&a_sel)
                        .next()
                        .map(|a| clean_inline_text(&a))
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| clean_inline_text(&el));
                    if !name.is_empty() {
                        pending_name = Some(name);
                    }
                }
                "dd" => {
                    let desc = clean_inline_text(&el);
                    if let Some(name) = pending_name.take() {
                        if desc.is_empty() {
                            items.push(name);
                        } else {
                            items.push(format!("{name}: {desc}"));
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(last) = pending_name.take() {
            items.push(last);
        }

        if items.len() > usize::from(heading.eq_ignore_ascii_case("modules")) {
            sections.push(DocSection {
                title: heading,
                descriptions: items,
                codes: Vec::new(),
                code_langs: Vec::new(),
                source: None,
            });
        }
    }
}

fn is_rst_underline(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 3 {
        return false;
    }
    let Some(first) = trimmed.chars().next() else {
        return false;
    };
    matches!(first, '=' | '-' | '~' | '^' | '*' | '+' | '#')
        && trimmed.chars().all(|ch| ch == first)
}

fn is_rst_directive_noise(trimmed: &str) -> bool {
    trimmed.starts_with(".. image::")
        || trimmed.starts_with(".. figure::")
        || trimmed.starts_with(".. raw::")
        || trimmed.starts_with(".. toctree::")
        || trimmed.starts_with(".. rubric::")
        || trimmed.starts_with(".. contents::")
        || trimmed.starts_with(":target:")
        || trimmed.starts_with(":alt:")
        || trimmed.starts_with(":width:")
        || trimmed.starts_with(":height:")
        || trimmed.starts_with(":align:")
        || trimmed.starts_with(":class:")
        || trimmed.starts_with(":scale:")
}

enum RstCodeDirective {
    None,
    Start(Option<String>),
}

fn parse_rst_code_directive(trimmed: &str) -> RstCodeDirective {
    let Some(rest) = trimmed
        .strip_prefix(".. code-block::")
        .or_else(|| trimmed.strip_prefix(".. sourcecode::"))
        .or_else(|| trimmed.strip_prefix(".. code::"))
    else {
        return RstCodeDirective::None;
    };
    let lang = rest.split_whitespace().next().unwrap_or("");
    if lang.is_empty() {
        RstCodeDirective::Start(None)
    } else {
        RstCodeDirective::Start(Some(lang.to_ascii_lowercase()))
    }
}

fn extract_fence_lang(trimmed_fence: &str) -> Option<String> {
    let info = trimmed_fence.trim_start_matches('`').trim();
    let lang = info
        .split(|ch: char| ch.is_whitespace() || ch == ',' || ch == '{')
        .next()
        .unwrap_or("")
        .trim();
    if lang.is_empty() {
        None
    } else {
        Some(lang.to_ascii_lowercase())
    }
}

struct MarkdownBuilder {
    sections: Vec<DocSection>,
    cur_title: String,
    cur_desc: Vec<String>,
    cur_code: Vec<String>,
    cur_code_langs: Vec<Option<String>>,
    source: Option<String>,
    current_h1: Option<String>,
}

impl MarkdownBuilder {
    fn new(lib_name: &str, summary: &str, source_label: &str) -> Self {
        let mut cur_desc = Vec::new();
        if !summary.trim().is_empty() {
            cur_desc.push(summary.trim().to_string());
        }
        Self {
            sections: Vec::new(),
            cur_title: format!("{lib_name} Overview"),
            cur_desc,
            cur_code: Vec::new(),
            cur_code_langs: Vec::new(),
            source: (!source_label.is_empty()).then(|| source_label.to_string()),
            current_h1: None,
        }
    }

    fn flush(&mut self) {
        if !self.cur_desc.is_empty() || !self.cur_code.is_empty() {
            self.sections.push(DocSection {
                title: self.cur_title.clone(),
                descriptions: std::mem::take(&mut self.cur_desc),
                codes: std::mem::take(&mut self.cur_code),
                code_langs: std::mem::take(&mut self.cur_code_langs),
                source: self.source.clone(),
            });
        }
    }

    fn push_code(&mut self, buf: &mut Vec<String>, lang: &mut Option<String>) {
        let block = buf.join("\n").trim().to_string();
        buf.clear();
        if block.is_empty() {
            *lang = None;
        } else {
            self.cur_code.push(block);
            self.cur_code_langs.push(lang.take());
        }
    }

    fn set_heading(&mut self, raw: &str, level: usize) {
        self.flush();
        let heading = clean_heading_text(raw);
        if heading.is_empty() {
            return;
        }
        if level == 1 {
            self.current_h1 = Some(heading.clone());
            self.cur_title = heading;
        } else if let Some(h1) = &self.current_h1 {
            if heading
                .to_ascii_lowercase()
                .contains(&h1.to_ascii_lowercase())
            {
                self.cur_title = heading;
            } else {
                self.cur_title = format!("{h1} > {heading}");
            }
        } else {
            self.cur_title = heading;
        }
    }
}

pub(crate) fn parse_markdown_sections(
    md: &str,
    summary: &str,
    source_label: &str,
    lib_name: &str,
) -> Vec<DocSection> {
    let mut builder = MarkdownBuilder::new(lib_name, summary, source_label);
    let mut in_code_block = false;
    let mut current_fence_lang: Option<String> = None;
    let mut in_rst_code = false;
    let mut rst_code_lang: Option<String> = None;
    let mut code_buf: Vec<String> = Vec::new();

    let lines: Vec<&str> = md.lines().collect();
    let mut idx = 0_usize;

    while idx < lines.len() {
        let line = lines[idx];
        let trimmed = line.trim();

        if trimmed.starts_with("```") {
            if in_code_block {
                builder.push_code(&mut code_buf, &mut current_fence_lang);
                in_code_block = false;
            } else {
                in_code_block = true;
                current_fence_lang = extract_fence_lang(trimmed);
            }
            idx += 1;
            continue;
        }

        if in_code_block {
            code_buf.push(line.to_string());
            idx += 1;
            continue;
        }

        if in_rst_code {
            if trimmed.is_empty() {
                if !code_buf.is_empty() {
                    code_buf.push(String::new());
                }
                idx += 1;
                continue;
            }
            if line.starts_with(' ') || line.starts_with('\t') {
                if !trimmed.starts_with(':') {
                    code_buf.push(line.to_string());
                }
                idx += 1;
                continue;
            }
            builder.push_code(&mut code_buf, &mut rst_code_lang);
            in_rst_code = false;
        }

        if let RstCodeDirective::Start(lang_opt) = parse_rst_code_directive(trimmed) {
            in_rst_code = true;
            rst_code_lang = lang_opt;
            code_buf.clear();
            idx += 1;
            continue;
        }

        if trimmed.starts_with('#') {
            let level = trimmed.bytes().take_while(|&b| b == b'#').count();
            builder.set_heading(trimmed.trim_start_matches('#'), level);
            idx += 1;
            continue;
        }

        if !trimmed.is_empty()
            && !is_rst_underline(trimmed)
            && !trimmed.starts_with("..")
            && idx + 1 < lines.len()
            && is_rst_underline(lines[idx + 1])
        {
            let underline_ch = lines[idx + 1].trim().chars().next().unwrap_or('-');
            let level = if underline_ch == '=' { 1 } else { 2 };
            // Avoid H1>H2 prefix when RST uses `---` for top-level subsections without H1 repetition.
            if level == 1 {
                builder.set_heading(trimmed, 1);
            } else {
                builder.flush();
                let heading = clean_heading_text(trimmed);
                if !heading.is_empty() {
                    builder.cur_title = heading;
                }
            }
            idx += 2;
            continue;
        }

        if !trimmed.is_empty()
            && !is_rst_underline(trimmed)
            && !is_rst_directive_noise(trimmed)
            && !trimmed.starts_with("[![")
            && !trimmed.starts_with("<img")
            && !trimmed.starts_with("<br")
            && !(trimmed.starts_with('<') && trimmed.ends_with('>'))
        {
            let cleaned = strip_html_tags(trimmed);
            let cleaned = cleaned.trim();
            if !cleaned.is_empty() {
                builder.cur_desc.push(cleaned.to_string());
            }
        }
        idx += 1;
    }

    if in_rst_code {
        builder.push_code(&mut code_buf, &mut rst_code_lang);
    }
    builder.flush();
    builder.sections
}

fn parse_markdown_doc(
    md: &str,
    summary: &str,
    source_url: &str,
    ecosystem: Ecosystem,
    lib_name: &str,
) -> ParsedDoc {
    let sections = parse_markdown_sections(md, summary, "", lib_name);
    ParsedDoc {
        sections,
        fallback: String::new(),
        source_url: source_url.to_string(),
        lang: ecosystem.as_str().to_string(),
        metadata: Vec::new(),
    }
}

/// Format HTML documentation into Context7-style structured snippets:
/// `TITLE / DESCRIPTION / SOURCE / LANGUAGE / CODE` separated by horizontal dividers.
#[must_use]
pub fn html_to_context7(
    html: &str,
    source_url: &str,
    ecosystem: Ecosystem,
    lib_name: &str,
) -> String {
    html_to_context7_with_options(html, source_url, ecosystem, lib_name, None, None)
}

/// Format HTML documentation with optional topic ranking/filtering and configurable output budget.
#[must_use]
pub fn html_to_context7_with_options(
    html: &str,
    source_url: &str,
    ecosystem: Ecosystem,
    lib_name: &str,
    topic: Option<&str>,
    max_chars: Option<usize>,
) -> String {
    let parsed = parse_html_doc(html, source_url, ecosystem, lib_name);
    parsed.render(topic, clamp_max_chars(max_chars))
}

/// Convert raw Markdown (for example from the `PyPI` JSON API) into Context7-style blocks.
#[must_use]
pub fn markdown_to_context7(
    md: &str,
    summary: &str,
    source_url: &str,
    ecosystem: Ecosystem,
    lib_name: &str,
) -> String {
    markdown_to_context7_with_options(md, summary, source_url, ecosystem, lib_name, None, None)
}

fn parse_pypi_json_doc(body: &str, fallback_url: &str, lib_name: &str) -> Option<ParsedDoc> {
    let json = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let info = &json["info"];
    let version = info["version"].as_str().unwrap_or("");
    let summary = info["summary"].as_str().unwrap_or("");
    let desc = info["description"].as_str().unwrap_or("");
    let page_url = info["package_url"]
        .as_str()
        .filter(|u| !u.is_empty())
        .unwrap_or(fallback_url);

    let mut parsed = parse_markdown_doc(desc, summary, page_url, Ecosystem::Python, lib_name);

    let mut meta_lines = Vec::new();
    if !version.is_empty() {
        meta_lines.push(format!("Resolved PyPI version: {version}"));
    }
    if let Some(urls) = info["project_urls"].as_object() {
        let mut pairs: Vec<(&String, &str)> = urls
            .iter()
            .filter_map(|(k, v)| v.as_str().filter(|s| !s.is_empty()).map(|s| (k, s)))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (label, href) in pairs {
            meta_lines.push(format!("{label}: {href}"));
        }
    } else if let Some(home) = info["home_page"].as_str().filter(|s| !s.is_empty()) {
        meta_lines.push(format!("Homepage: {home}"));
    }

    parsed.metadata = meta_lines;
    Some(parsed)
}

/// Parse a `PyPI` JSON API payload into Context7-style Markdown, surfacing version metadata and
/// official `project_urls` (such as `Documentation` / `ReadTheDocs` links).
#[must_use]
pub fn parse_pypi_json_to_context7(
    body: &str,
    fallback_url: &str,
    lib_name: &str,
    topic: Option<&str>,
    max_chars: Option<usize>,
) -> Option<String> {
    let parsed = parse_pypi_json_doc(body, fallback_url, lib_name)?;
    Some(parsed.render(topic, clamp_max_chars(max_chars)))
}

/// Convert raw Markdown into Context7-style blocks with optional topic filtering and output budget.
#[must_use]
pub fn markdown_to_context7_with_options(
    md: &str,
    summary: &str,
    source_url: &str,
    ecosystem: Ecosystem,
    lib_name: &str,
    topic: Option<&str>,
    max_chars: Option<usize>,
) -> String {
    let parsed = parse_markdown_doc(md, summary, source_url, ecosystem, lib_name);
    parsed.render(topic, clamp_max_chars(max_chars))
}

fn tokenize_words(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn tokenize_topic(topic: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for token in tokenize_words(topic) {
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    tokens
}

/// Count `(exact_word_hits, prefix_word_hits)` for `token` in `words`.
/// Prefix matches require `token.len() >= 4` and the candidate word to be strictly longer than
/// `token` (so `"bounded"` does not spuriously match `"bound"` in `"CPU-bound"`).
fn count_word_hits(words: &[String], token: &str) -> (usize, usize) {
    let mut exact = 0_usize;
    let mut prefix = 0_usize;
    for word in words {
        if word == token {
            exact += 1;
        } else if token.len() >= 4 && word.starts_with(token) {
            prefix += 1;
        }
    }
    (exact, prefix)
}

fn score_line(line: &str, tokens: &[String]) -> usize {
    let words = tokenize_words(line);
    let mut score = 0_usize;
    let mut distinct = 0_usize;
    for token in tokens {
        let (exact, prefix) = count_word_hits(&words, token);
        if exact > 0 || prefix > 0 {
            distinct += 1;
            score += exact.min(3) * 6 + prefix.min(2) * 2;
        }
    }
    score + distinct * distinct * 8
}

fn is_changelog_title_or_source(section: &DocSection) -> bool {
    if let Some(src) = &section.source {
        let lower_src = src.to_ascii_lowercase();
        if lower_src.contains("changelog")
            || lower_src.contains("migration")
            || lower_src.contains("history.md")
            || lower_src.contains("changes.md")
        {
            return true;
        }
    }

    let lower_title = section.title.to_ascii_lowercase();
    if lower_title.contains("changelog")
        || lower_title.contains("release notes")
        || lower_title.contains("migration")
        || matches!(
            lower_title.as_str(),
            "fixed" | "fixes" | "changed" | "added" | "removed" | "deprecated" | "bug fixes"
        )
    {
        return true;
    }

    // Headings that start with a semver release number (e.g. `0.13.0`, `v1.2.3`, `Version 2.0`)
    let stripped = lower_title
        .trim_start_matches("version ")
        .trim_start_matches(['v', '[', '(']);
    let mut segs = stripped.split('.');
    matches!(
        (segs.next(), segs.next()),
        (Some(a), Some(b))
            if !a.is_empty()
                && a.chars().all(|c| c.is_ascii_digit())
                && !b.is_empty()
                && b.chars().next().is_some_and(|c| c.is_ascii_digit())
    )
}

fn score_section(section: &DocSection, tokens: &[String]) -> usize {
    let title_words = tokenize_words(&section.title);
    let source_words = section.source.as_deref().map_or_default(tokenize_words);
    let desc_words = tokenize_words(&section.descriptions.join(" "));
    let code_words = tokenize_words(&section.codes.join("\n"));

    let mut score = 0_usize;
    let mut distinct_matched = 0_usize;

    for token in tokens {
        let (t_exact, t_prefix) = count_word_hits(&title_words, token);
        let (s_exact, s_prefix) = count_word_hits(&source_words, token);
        let (c_exact, c_prefix) = count_word_hits(&code_words, token);
        let (d_exact, d_prefix) = count_word_hits(&desc_words, token);

        let token_score = t_exact.min(2) * 25
            + t_prefix.min(2) * 10
            + s_exact.min(2) * 20
            + s_prefix.min(2) * 8
            + c_exact.min(3) * 8
            + c_prefix.min(2) * 3
            + d_exact.min(4) * 5
            + d_prefix.min(3) * 2;

        if token_score > 0 {
            distinct_matched += 1;
            score += token_score;
        }
    }

    if score > 0 {
        // Strongly reward sections covering multiple distinct query terms.
        score += distinct_matched * distinct_matched * 15;
        if !section.codes.is_empty() {
            score += 4;
        }
        // Demote changelog/migration entries so real documentation ranks ahead of release history.
        if is_changelog_title_or_source(section) {
            score = (score / 4).max(1);
        }
    }
    score
}

fn default_section_order(sections: &[DocSection]) -> Vec<&DocSection> {
    let mut ordered: Vec<&DocSection> = Vec::new();
    if let Some(first) = sections.first() {
        ordered.push(first);
    }
    for section in sections.iter().skip(1) {
        if !section.codes.is_empty() {
            ordered.push(section);
        }
    }
    for section in sections.iter().skip(1) {
        if section.codes.is_empty() && !section.descriptions.is_empty() {
            ordered.push(section);
        }
    }
    ordered
}

/// Slice UTF-8 text cleanly at byte `offset` up to `max_chars` bytes, aligning on UTF-8 and
/// word/line boundaries and appending pagination metadata when truncated or offset.
#[must_use]
pub fn slice_with_pagination(input: &str, offset: usize, max_chars: usize) -> String {
    let total = input.len();
    if offset == 0 && total <= max_chars {
        return input.to_string();
    }
    if offset >= total {
        return format!(
            "[offset {offset} exceeds document length {total}; pass offset=0 to start from the beginning]"
        );
    }

    let mut start = offset;
    while start < total && !input.is_char_boundary(start) {
        start += 1;
    }

    let mut end = start.saturating_add(max_chars).min(total);
    while end > start && !input.is_char_boundary(end) {
        end -= 1;
    }

    if end < total {
        let slice = &input[start..end];
        if let Some(rel) = slice.rfind('\n').or_else(|| slice.rfind(' ')) {
            if rel > 0 {
                end = start + rel;
            }
        }
    }

    let body = &input[start..end];
    if end < total {
        format!(
            "{body}\n\n[showing bytes {start}..{end} of {total}; pass offset={end} to read more]"
        )
    } else {
        format!("{body}\n\n[showing bytes {start}..{end} of {total}; end of document]")
    }
}

fn format_single_section(
    section: &DocSection,
    default_source: &str,
    lang: &str,
    tokens: &[String],
) -> String {
    let mut block = String::new();
    let _ = writeln!(block, "TITLE: {}", section.title);
    if !section.descriptions.is_empty() {
        let selected_lines: Vec<&str> = if !tokens.is_empty() && section.descriptions.len() > 4 {
            let mut scored_lines: Vec<(usize, usize, &str)> = section
                .descriptions
                .iter()
                .enumerate()
                .filter_map(|(idx, line)| {
                    let s = score_line(line, tokens);
                    (s > 0).then_some((s, idx, line.as_str()))
                })
                .collect();
            if scored_lines.is_empty() {
                section.descriptions.iter().map(String::as_str).collect()
            } else {
                scored_lines.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
                scored_lines.into_iter().map(|(_, _, line)| line).collect()
            }
        } else {
            section.descriptions.iter().map(String::as_str).collect()
        };
        let _ = writeln!(block, "DESCRIPTION:\n{}", selected_lines.join("\n"));
    }

    let src = section.source.as_deref().unwrap_or(default_source);
    let _ = writeln!(block, "SOURCE: {src}");
    let _ = writeln!(block, "LANGUAGE: {lang}");

    if !section.codes.is_empty() {
        let mut indexed: Vec<(usize, usize, &str, &str)> = section
            .codes
            .iter()
            .enumerate()
            .map(|(idx, code)| {
                let fence = section
                    .code_langs
                    .get(idx)
                    .and_then(Option::as_deref)
                    .unwrap_or(lang);
                let s = if tokens.is_empty() {
                    0
                } else {
                    score_line(code, tokens)
                };
                (s, idx, code.as_str(), fence)
            })
            .collect();
        if !tokens.is_empty() && indexed.len() > 2 {
            indexed.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        }

        block.push_str("CODE:\n");
        for (_, _, code, fence_lang) in indexed.into_iter().take(3) {
            let _ = writeln!(block, "```{fence_lang}\n{code}\n```");
        }
    }
    block.push_str("----------------------------------------\n\n");
    block
}

fn format_sections_all(
    sections: &[DocSection],
    default_source: &str,
    lang: &str,
    topic: Option<&str>,
    max_total: usize,
) -> String {
    let mut out = String::new();
    let tokens = topic.map_or_default(tokenize_topic);

    let ordered: Vec<&DocSection> = if tokens.is_empty() {
        default_section_order(sections)
    } else {
        let mut scored: Vec<(usize, usize, &DocSection)> = Vec::new();
        for (idx, section) in sections.iter().enumerate() {
            let score = score_section(section, &tokens);
            if score > 0 {
                scored.push((score, idx, section));
            }
        }
        if scored.is_empty() {
            let raw_topic = topic.map_or("", str::trim);
            let _ = writeln!(out, "No sections matched topic '{raw_topic}'.");
            if !sections.is_empty() {
                out.push_str("\nAvailable sections:\n");
                let mut seen = Vec::new();
                for section in sections.iter().take(40) {
                    if !seen.contains(&section.title) {
                        seen.push(section.title.clone());
                        let _ = writeln!(out, "- {}", section.title);
                    }
                }
                out.push_str(
                    "\nTip: retry without `topic` or with one of the section titles above.",
                );
            }
            return out.trim().to_string();
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, _, section)| section).collect()
    };

    for section in ordered {
        let block = format_single_section(section, default_source, lang, &tokens);
        if out.len() + block.len() > max_total && !out.is_empty() {
            break;
        }
        out.push_str(&block);
    }

    out.trim().to_string()
}

pub(crate) fn render_doc_sections_paginated(
    sections: &[DocSection],
    default_source: &str,
    lang: &str,
    topic: Option<&str>,
    max_chars: usize,
    offset: usize,
) -> String {
    let full = format_sections_all(sections, default_source, lang, topic, 256 * 1024);
    slice_with_pagination(&full, offset, max_chars)
}

/// Remove inline HTML tag markup from a text line (preserving standalone `>` such as `->`).
#[must_use]
pub fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            other if !in_tag => out.push(other),
            _ => {}
        }
    }
    out
}

fn clean_inline_text(el: &scraper::ElementRef<'_>) -> String {
    let mut out = String::new();
    for node in el.text() {
        let trimmed = node.split_whitespace().collect::<Vec<_>>().join(" ");
        if !trimmed.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&trimmed);
        }
    }
    out
}

/// Truncate UTF-8 text cleanly at a character and word/line boundary.
#[must_use]
pub fn truncate_smart(input: &str, max: usize) -> String {
    if input.len() <= max {
        return input.to_string();
    }
    let mut end = max;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    let slice = &input[..end];
    let boundary = slice
        .rfind('\n')
        .or_else(|| slice.rfind(' '))
        .unwrap_or(end);
    format!("{}\n... (truncated)", &slice[..boundary])
}

fn build_http_client() -> reqwest::Client {
    let redirect_policy = Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        if let Err(reason) = validate_parsed_url(attempt.url()) {
            return attempt.error(format!("blocked redirect: {reason}"));
        }
        attempt.follow()
    });

    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(redirect_policy)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

#[derive(Clone)]
pub struct FastDocsServer {
    tool_router: ToolRouter<Self>,
    client: reqwest::Client,
    cache: Option<DiskCache>,
}

impl Default for FastDocsServer {
    fn default() -> Self {
        Self::new()
    }
}

impl FastDocsServer {
    async fn fetch_allowlisted(&self, url: &str) -> Result<String, String> {
        validate_outbound_url(url)?;
        fetch_url_with_limits(&self.client, url, REQUEST_TIMEOUT, MAX_RESPONSE_BYTES).await
    }

    async fn fetch_allowlisted_conditional(
        &self,
        url: &str,
        if_none_match: Option<&str>,
    ) -> Result<FetchOutcome, String> {
        validate_outbound_url(url)?;
        fetch_url_conditional(
            &self.client,
            url,
            if_none_match,
            REQUEST_TIMEOUT,
            MAX_RESPONSE_BYTES,
        )
        .await
    }

    async fn search_rust(&self, query: &str) -> Result<Option<String>, String> {
        let url = format!("https://docs.rs/releases/search?query={query}");
        let html = self.fetch_allowlisted(&url).await?;
        let doc = scraper::Html::parse_document(&html);

        let (Ok(rel_sel), Ok(name_sel), Ok(desc_sel)) = (
            scraper::Selector::parse("a.release"),
            scraper::Selector::parse("div.name"),
            scraper::Selector::parse("div.description"),
        ) else {
            return Ok(None);
        };

        let mut entries = Vec::new();
        for el in doc.select(&rel_sel).take(8) {
            let Some(name_el) = el.select(&name_sel).next() else {
                continue;
            };
            let raw_name = clean_inline_text(&name_el);
            let (crate_name, ver) = split_rust_release_name(&raw_name);
            let desc = el
                .select(&desc_sel)
                .next()
                .map_or_default(|d| clean_inline_text(&d));
            let label = match ver {
                Some(v) => format!("**{crate_name}** (rust, {v})"),
                None => format!("**{crate_name}** (rust)"),
            };
            if desc.is_empty() {
                entries.push(format!("- {label}"));
            } else {
                entries.push(format!("- {label}: {desc}"));
            }
        }

        if entries.is_empty() {
            Ok(None)
        } else {
            Ok(Some(format!("## Rust (docs.rs)\n{}", entries.join("\n"))))
        }
    }

    async fn search_python(
        &self,
        raw_query: &str,
        encoded_query: &str,
    ) -> Result<Option<String>, String> {
        let exact_fut = async {
            let candidate = extract_pypi_candidate(raw_query)?;
            let Ok(exact_url) = build_url(candidate, "latest", Ecosystem::Python) else {
                return None;
            };
            let body = self.fetch_allowlisted(&exact_url).await.ok()?;
            let json = serde_json::from_str::<serde_json::Value>(&body).ok()?;
            let name = json["info"]["name"].as_str().unwrap_or(candidate);
            let ver = json["info"]["version"].as_str().unwrap_or("latest");
            let sum = json["info"]["summary"].as_str().unwrap_or("");
            Some((
                name.to_ascii_lowercase(),
                format!("- **{name}** (python, {ver}, PyPI exact): {sum}"),
            ))
        };

        let gh_url = format!(
            "https://api.github.com/search/repositories?q={encoded_query}+language:python&sort=stars&per_page=6"
        );
        let gh_fut = self.fetch_allowlisted(&gh_url);

        let (exact_entry, gh_res) = tokio::join!(exact_fut, gh_fut);

        let mut py_entries: Vec<String> = Vec::new();
        let mut exact_pkg_lower: Option<String> = None;
        if let Some((pkg_lower, formatted)) = exact_entry {
            exact_pkg_lower = Some(pkg_lower);
            py_entries.push(formatted);
        }

        match gh_res {
            Ok(body) => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                    if let Some(items) = json["items"].as_array() {
                        for item in items.iter().take(6) {
                            let short_name = item["name"].as_str().unwrap_or("");
                            let full_name = item["full_name"].as_str().unwrap_or(short_name);
                            let desc = item["description"].as_str().unwrap_or("");
                            if short_name.is_empty() {
                                continue;
                            }
                            if exact_pkg_lower
                                .as_deref()
                                .is_some_and(|e| e.eq_ignore_ascii_case(short_name))
                            {
                                continue;
                            }
                            if desc.is_empty() {
                                py_entries.push(format!("- **{full_name}** (python / GitHub)"));
                            } else {
                                py_entries
                                    .push(format!("- **{full_name}** (python / GitHub): {desc}"));
                            }
                        }
                    }
                }
            }
            Err(err) if py_entries.is_empty() => return Err(err),
            Err(_) => {}
        }

        if py_entries.is_empty() {
            Ok(None)
        } else {
            Ok(Some(format!(
                "## Python (PyPI / GitHub)\n{}",
                py_entries.join("\n")
            )))
        }
    }

    async fn search_go(&self, query: &str) -> Result<Option<String>, String> {
        let url = format!("https://pkg.go.dev/search?q={query}");
        let html = self.fetch_allowlisted(&url).await?;
        let doc = scraper::Html::parse_document(&html);

        let (Ok(snip_sel), Ok(a_sel), Ok(syn_sel)) = (
            scraper::Selector::parse("div.SearchSnippet"),
            scraper::Selector::parse("a"),
            scraper::Selector::parse("p.SearchSnippet-synopsis"),
        ) else {
            return Ok(None);
        };

        let mut entries = Vec::new();
        for el in doc.select(&snip_sel).take(8) {
            let Some(anchor) = el.select(&a_sel).next() else {
                continue;
            };
            let header = clean_inline_text(&anchor);
            let syn = el
                .select(&syn_sel)
                .next()
                .map_or_default(|p| clean_inline_text(&p));
            if syn.is_empty() {
                entries.push(format!("- **{header}** (go)"));
            } else {
                entries.push(format!("- **{header}** (go): {syn}"));
            }
        }

        if entries.is_empty() {
            Ok(None)
        } else {
            Ok(Some(format!("## Go (pkg.go.dev)\n{}", entries.join("\n"))))
        }
    }

    async fn search_npm(&self, encoded_query: &str) -> Result<Option<String>, String> {
        let url = format!("https://registry.npmjs.org/-/v1/search?text={encoded_query}&size=8");
        let body = self.fetch_allowlisted(&url).await?;
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) else {
            return Ok(None);
        };
        let Some(objects) = json["objects"].as_array() else {
            return Ok(None);
        };

        let mut entries = Vec::new();
        for obj in objects.iter().take(8) {
            let pkg = &obj["package"];
            let name = pkg["name"].as_str().unwrap_or("");
            let ver = pkg["version"].as_str().unwrap_or("latest");
            let desc = pkg["description"].as_str().unwrap_or("");
            if !name.is_empty() {
                entries.push(format!("- **{name}@{ver}** (npm): {desc}"));
            }
        }

        if entries.is_empty() {
            Ok(None)
        } else {
            Ok(Some(format!(
                "## JavaScript / TypeScript (npm)\n{}",
                entries.join("\n")
            )))
        }
    }

    async fn fetch_npm_fallback(
        &self,
        library: &str,
        version: &str,
    ) -> Result<(ParsedDoc, Option<String>), String> {
        let lower_url = format!("https://unpkg.com/{library}@{version}/readme.md");
        if let Ok(FetchOutcome::Fetched {
            body,
            etag,
            final_url,
        }) = self.fetch_allowlisted_conditional(&lower_url, None).await
        {
            return Ok((
                parse_markdown_doc(&body, "", &final_url, Ecosystem::Npm, library),
                etag,
            ));
        }

        let encoded_pkg = library.replace('/', "%2F");
        let registry_url = format!("https://registry.npmjs.org/{encoded_pkg}");
        let FetchOutcome::Fetched {
            body,
            etag,
            final_url,
        } = self
            .fetch_allowlisted_conditional(&registry_url, None)
            .await?
        else {
            return Err("unexpected 304 on npm registry fallback".to_string());
        };
        let json = serde_json::from_str::<serde_json::Value>(&body)
            .map_err(|err| format!("invalid npm registry JSON: {err}"))?;
        let summary = json["description"].as_str().unwrap_or("");
        let readme = json["readme"].as_str().unwrap_or("");
        let page_url = json["homepage"]
            .as_str()
            .filter(|u| !u.is_empty())
            .unwrap_or(&final_url);
        Ok((
            parse_markdown_doc(readme, summary, page_url, Ecosystem::Npm, library),
            etag,
        ))
    }

    /// Fallback for large `PyPI` packages (such as `ruff`) whose unpinned `/pypi/<pkg>/json`
    /// exceeds 5 MiB because of historical `releases`: fetch the ~12 KiB RSS releases feed to
    /// discover the latest version, then fetch the compact `/pypi/<pkg>/<ver>/json`.
    async fn fetch_pypi_via_rss_fallback(
        &self,
        library: &str,
    ) -> Result<(ParsedDoc, Option<String>), String> {
        let rss_url = format!("https://pypi.org/rss/project/{library}/releases.xml");
        let rss_body = self.fetch_allowlisted(&rss_url).await?;
        let latest_ver = extract_latest_version_from_pypi_rss(&rss_body)
            .ok_or_else(|| "could not determine latest version from PyPI RSS feed".to_string())?;
        let pinned_url = build_url(library, &latest_ver, Ecosystem::Python)?;
        let FetchOutcome::Fetched {
            body,
            etag,
            final_url,
        } = self
            .fetch_allowlisted_conditional(&pinned_url, None)
            .await?
        else {
            return Err("unexpected 304 on PyPI versioned fallback".to_string());
        };
        let parsed = parse_pypi_json_doc(&body, &final_url, library)
            .unwrap_or_else(|| parse_html_doc(&body, &final_url, Ecosystem::Python, library));
        Ok((parsed, etag))
    }

    /// Fallback for `docs.rs` Rust `::` item paths (e.g. `builder::Command` -> `struct.Command.html`).
    async fn fetch_rustdoc_item_fallback(
        &self,
        library: &str,
        version: &str,
        module: &str,
    ) -> Option<(ParsedDoc, Option<String>)> {
        let candidates = rustdoc_item_fallback_urls(library, version, module);
        if candidates.len() != 3 {
            return None;
        }
        let (r0, r1, r2) = tokio::join!(
            self.fetch_allowlisted_conditional(&candidates[0], None),
            self.fetch_allowlisted_conditional(&candidates[1], None),
            self.fetch_allowlisted_conditional(&candidates[2], None),
        );
        for outcome in [r0, r1, r2] {
            if let Ok(FetchOutcome::Fetched {
                body,
                etag,
                final_url,
            }) = outcome
            {
                return Some((
                    parse_html_doc(&body, &final_url, Ecosystem::Rust, library),
                    etag,
                ));
            }
        }
        None
    }
}

#[tool_router]
impl FastDocsServer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            client: build_http_client(),
            cache: DiskCache::default_user_cache(),
        }
    }

    #[tool(
        name = "fetch_docs",
        description = "Fetch documentation and code examples for a library, optionally targeting a submodule/item path, filtering sections by topic keywords, and auto-resolving the library version from local project manifests (Cargo.lock, Cargo.toml, package.json, pyproject.toml, go.mod). Supports Rust, Python, Go, and npm/JS/TS with local ETag caching."
    )]
    async fn fetch_docs(
        &self,
        params: Parameters<FetchDocsRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = params.0;
        let project_dir_clean = req
            .project_dir
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty() && !d.contains('\0'));

        let explicit_ecosystem = match parse_ecosystem_arg(req.lang.as_deref()) {
            Ok(opt) => opt,
            Err(err) => {
                return Ok(CallToolResult::success(vec![ContentBlock::text(err)]));
            }
        };

        let ecosystem = explicit_ecosystem
            .or_else(|| {
                project_dir_clean
                    .and_then(|dir| detect_project_ecosystem(Path::new(dir), &req.library))
            })
            .unwrap_or_else(|| detect_lang(&req.library));

        // For Go short names (e.g. `pflag`), resolve full module path from `go.mod` when unique.
        let effective_library = if ecosystem == Ecosystem::Go && !req.library.contains('/') {
            project_dir_clean
                .and_then(|dir| resolve_go_short_name(Path::new(dir), &req.library))
                .unwrap_or_else(|| req.library.clone())
        } else {
            req.library.clone()
        };

        let mut resolved_source: Option<&'static str> = None;
        let version = match req
            .version
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            Some(explicit) => explicit.to_string(),
            None => {
                if let Some(dir) = project_dir_clean {
                    if let Some(resolved) =
                        resolve_project_version(Path::new(dir), &effective_library, ecosystem)
                    {
                        resolved_source = Some(resolved.source_file);
                        resolved.version
                    } else {
                        "latest".to_string()
                    }
                } else {
                    "latest".to_string()
                }
            }
        };
        let is_pinned = !version.is_empty() && version != "latest";
        let max_chars = clamp_max_chars(req.max_chars);
        let offset = req.offset.unwrap_or(0);

        let url = match build_url_with_module(
            &effective_library,
            &version,
            ecosystem,
            req.module.as_deref(),
        ) {
            Ok(url) => url,
            Err(err) => {
                return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Invalid request for '{}': {err}",
                    req.library
                ))]));
            }
        };

        // PyPI and npm package READMEs are single-document sources; fold any `module` hint into `topic`.
        let effective_topic: Option<String> =
            match (ecosystem, req.module.as_deref(), req.topic.as_deref()) {
                (Ecosystem::Python | Ecosystem::Npm, Some(module), Some(topic)) => {
                    Some(format!("{module} {topic}"))
                }
                (Ecosystem::Python | Ecosystem::Npm, Some(module), None) => {
                    Some(module.to_string())
                }
                (_, _, Some(topic)) => Some(topic.to_string()),
                (_, _, None) => None,
            };

        let base_label = match resolved_source {
            Some(src) => format!("{}@{version} [from {src}]", req.library),
            None if is_pinned => format!("{}@{version}", req.library),
            None => req.library.clone(),
        };
        let label = match req
            .module
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            Some(module) => format!("{base_label} ({module})"),
            None => base_label,
        };

        let cached_entry = self
            .cache
            .as_ref()
            .and_then(|cache| cache.lookup(&url, is_pinned));

        let mut stale_parsed: Option<ParsedDoc> = None;
        let mut stale_etag: Option<String> = None;

        if let Some(entry) = cached_entry {
            if let Ok(parsed) = serde_json::from_str::<ParsedDoc>(&entry.content) {
                if entry.is_fresh {
                    let rendered =
                        parsed.render_paginated(effective_topic.as_deref(), max_chars, offset);
                    return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                        "# {label} ({}, cached)\n\n{rendered}",
                        ecosystem.as_str()
                    ))]));
                }
                stale_parsed = Some(parsed);
                stale_etag = entry.etag;
            }
        }

        match self
            .fetch_allowlisted_conditional(&url, stale_etag.as_deref())
            .await
        {
            Ok(FetchOutcome::NotModified { etag }) => {
                if let Some(parsed) = stale_parsed {
                    if let Some(cache) = &self.cache {
                        let _ = cache.refresh(&url, is_pinned, etag.as_deref());
                    }
                    let rendered =
                        parsed.render_paginated(effective_topic.as_deref(), max_chars, offset);
                    return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                        "# {label} (revalidated)\n\n{rendered}"
                    ))]));
                }
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Documentation not found for {label} ({}): empty revalidated cache",
                    ecosystem.as_str()
                ))]))
            }
            Ok(FetchOutcome::Fetched {
                body,
                etag,
                final_url,
            }) => {
                let parsed = match ecosystem {
                    Ecosystem::Python => parse_pypi_json_doc(&body, &final_url, &req.library)
                        .unwrap_or_else(|| {
                            parse_html_doc(&body, &final_url, ecosystem, &req.library)
                        }),
                    Ecosystem::Npm => {
                        parse_markdown_doc(&body, "", &final_url, Ecosystem::Npm, &req.library)
                    }
                    Ecosystem::Rust | Ecosystem::Go => {
                        parse_html_doc(&body, &final_url, ecosystem, &req.library)
                    }
                };

                if let Some(cache) = &self.cache {
                    if let Ok(serialized) = serde_json::to_string(&parsed) {
                        if let Err(err) =
                            cache.put_with_etag(&url, &serialized, is_pinned, etag.as_deref())
                        {
                            tracing::warn!("failed to write cache entry: {err}");
                        }
                    }
                }

                let rendered =
                    parsed.render_paginated(effective_topic.as_deref(), max_chars, offset);
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "# {label} ({})\n\n{rendered}",
                    ecosystem.as_str()
                ))]))
            }
            Err(err) if ecosystem == Ecosystem::Npm => {
                let ver_norm = if version.is_empty() {
                    "latest"
                } else {
                    version.as_str()
                };
                match self.fetch_npm_fallback(&req.library, ver_norm).await {
                    Ok((parsed, etag)) => {
                        if let Some(cache) = &self.cache {
                            if let Ok(serialized) = serde_json::to_string(&parsed) {
                                let _ = cache.put_with_etag(
                                    &url,
                                    &serialized,
                                    is_pinned,
                                    etag.as_deref(),
                                );
                            }
                        }
                        let rendered =
                            parsed.render_paginated(effective_topic.as_deref(), max_chars, offset);
                        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                            "# {label} (npm)\n\n{rendered}"
                        ))]))
                    }
                    Err(fallback_err) => Ok(CallToolResult::success(vec![ContentBlock::text(
                        format!(
                            "Documentation not found for {label} (npm): {err}; fallback: {fallback_err}"
                        ),
                    )])),
                }
            }
            Err(err)
                if ecosystem == Ecosystem::Python
                    && !is_pinned
                    && err.contains("exceeds maximum allowed size") =>
            {
                match self.fetch_pypi_via_rss_fallback(&effective_library).await {
                    Ok((parsed, etag)) => {
                        if let Some(cache) = &self.cache {
                            if let Ok(serialized) = serde_json::to_string(&parsed) {
                                let _ = cache.put_with_etag(&url, &serialized, false, etag.as_deref());
                            }
                        }
                        let rendered =
                            parsed.render_paginated(effective_topic.as_deref(), max_chars, offset);
                        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                            "# {label} (python)\n\n{rendered}"
                        ))]))
                    }
                    Err(fb_err) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                        "Documentation not found for {label} (python): {err}; RSS fallback: {fb_err}"
                    ))])),
                }
            }
            Err(err)
                if ecosystem == Ecosystem::Rust && err == "HTTP 404" && req.module.is_some() =>
            {
                if let Some(module_str) = req.module.as_deref() {
                    if let Some((parsed, etag)) = self
                        .fetch_rustdoc_item_fallback(&effective_library, &version, module_str)
                        .await
                    {
                        if let Some(cache) = &self.cache {
                            if let Ok(serialized) = serde_json::to_string(&parsed) {
                                let _ = cache.put_with_etag(
                                    &url,
                                    &serialized,
                                    is_pinned,
                                    etag.as_deref(),
                                );
                            }
                        }
                        let rendered =
                            parsed.render_paginated(effective_topic.as_deref(), max_chars, offset);
                        return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                            "# {label} (rust)\n\n{rendered}"
                        ))]));
                    }
                }
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Documentation not found for {label} (rust): {err} (Tip: call fetch_docs without 'module' to inspect the library's available Modules/Items table.)"
                ))]))
            }
            Err(err) => {
                let module_hint = if err == "HTTP 404" && req.module.is_some() {
                    " (Tip: call fetch_docs without 'module' to inspect the library's available Modules/Items table.)"
                } else {
                    ""
                };
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Documentation not found for {label} ({}): {err}{module_hint}",
                    ecosystem.as_str()
                ))]))
            }
        }
    }

    #[tool(
        name = "search_docs",
        description = "Search documentation across Rust (docs.rs), Python (PyPI / GitHub), Go (pkg.go.dev), and npm (registry.npmjs.org) in parallel with 15-minute local caching. Returns structured package identifiers and descriptions."
    )]
    async fn search_docs(
        &self,
        params: Parameters<SearchDocsRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = params.0;
        let trimmed = req.query.trim();
        if trimmed.is_empty() || trimmed.len() > MAX_IDENTIFIER_LEN {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "Invalid search query: must be non-empty and at most 200 characters.",
            )]));
        }

        let scope = match SearchScope::parse_arg(req.lang.as_deref()) {
            Ok(scope) => scope,
            Err(err) => {
                return Ok(CallToolResult::success(vec![ContentBlock::text(err)]));
            }
        };
        let search_cache_key =
            format!("search:{}:{}", scope.as_key(), trimmed.to_ascii_lowercase());

        if let Some(cached) = self
            .cache
            .as_ref()
            .and_then(|cache| cache.get_with_ttl(&search_cache_key, SEARCH_CACHE_TTL))
        {
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "# Search: {trimmed} (cached)\n\n{cached}"
            ))]));
        }

        let encoded_query = urlencoding(trimmed);

        let rust_fut = async {
            if scope.includes(Ecosystem::Rust) {
                Some(self.search_rust(&encoded_query).await)
            } else {
                None
            }
        };
        let python_fut = async {
            if scope.includes(Ecosystem::Python) {
                Some(self.search_python(trimmed, &encoded_query).await)
            } else {
                None
            }
        };
        let go_fut = async {
            if scope.includes(Ecosystem::Go) {
                Some(self.search_go(&encoded_query).await)
            } else {
                None
            }
        };
        let npm_fut = async {
            if scope.includes(Ecosystem::Npm) {
                Some(self.search_npm(&encoded_query).await)
            } else {
                None
            }
        };

        let (rust_res, python_res, go_res, npm_res) =
            tokio::join!(rust_fut, python_fut, go_fut, npm_fut);

        let mut results = Vec::new();
        let mut errors = Vec::new();

        for (name, outcome) in [
            ("rust", rust_res),
            ("python", python_res),
            ("go", go_res),
            ("npm", npm_res),
        ] {
            if let Some(res) = outcome {
                match res {
                    Ok(Some(section)) => results.push(section),
                    Ok(None) => {}
                    Err(err) => errors.push(format!("{name}: {err}")),
                }
            }
        }

        if results.is_empty() {
            let text = if errors.is_empty() {
                format!("No results found for: {trimmed}")
            } else {
                format!(
                    "No results found for '{trimmed}' (errors: {})",
                    errors.join(", ")
                )
            };
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
        } else {
            let joined = results.join("\n\n");
            if let Some(cache) = &self.cache {
                if let Err(err) = cache.put(&search_cache_key, &joined, false) {
                    tracing::warn!("failed to write search cache entry: {err}");
                }
            }
            Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "# Search: {trimmed}\n\n{joined}"
            ))]))
        }
    }

    #[tool(
        name = "fetch_llms_txt",
        description = "Fetch and filter an official llms.txt or llms-full.txt documentation file from a public HTTPS documentation site (e.g. hono.dev, docs.pydantic.dev, vite.dev). Enforces DNS-pinned public-IP SSRF protection, 5 MiB streaming cap, topic filtering, and local ETag caching."
    )]
    async fn fetch_llms_txt(
        &self,
        params: Parameters<FetchLlmsTxtRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = params.0;
        let want_full = req.full.unwrap_or(false);
        let max_chars = clamp_max_chars(req.max_chars);
        let offset = req.offset.unwrap_or(0);

        let primary_url = match build_llms_txt_url(&req.target, want_full) {
            Ok(url) => url,
            Err(err) => {
                return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Invalid llms.txt target '{}': {err}",
                    req.target
                ))]));
            }
        };
        let url_str = primary_url.as_str().to_string();
        let Some(host) = primary_url.host_str().map(ToOwned::to_owned) else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "Invalid llms.txt target: missing host",
            )]));
        };

        let cached_entry = self
            .cache
            .as_ref()
            .and_then(|cache| cache.lookup(&url_str, false));

        let mut stale_parsed: Option<ParsedDoc> = None;
        let mut stale_etag: Option<String> = None;

        if let Some(entry) = cached_entry {
            if let Ok(parsed) = serde_json::from_str::<ParsedDoc>(&entry.content) {
                if entry.is_fresh {
                    let rendered = parsed.render_paginated(req.topic.as_deref(), max_chars, offset);
                    return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                        "# {host} llms.txt (cached)\n\n{rendered}"
                    ))]));
                }
                stale_parsed = Some(parsed);
                stale_etag = entry.etag;
            }
        }

        let outcome = fetch_llms_with_safe_redirects(
            &primary_url,
            stale_etag.as_deref(),
            USER_AGENT,
            CONNECT_TIMEOUT,
            REQUEST_TIMEOUT,
            MAX_RESPONSE_BYTES,
        )
        .await;

        let (final_url, final_outcome) = match outcome {
            Err(err) if want_full && err == "HTTP 404" => {
                if let Ok(fallback_url) = build_llms_txt_url(&req.target, false) {
                    let fb_str = fallback_url.as_str().to_string();
                    let fb_res = fetch_llms_with_safe_redirects(
                        &fallback_url,
                        None,
                        USER_AGENT,
                        CONNECT_TIMEOUT,
                        REQUEST_TIMEOUT,
                        MAX_RESPONSE_BYTES,
                    )
                    .await;
                    (fb_str, fb_res)
                } else {
                    (url_str, Err(err))
                }
            }
            other => (url_str, other),
        };

        match final_outcome {
            Ok(FetchOutcome::NotModified { etag }) => {
                if let Some(parsed) = stale_parsed {
                    if let Some(cache) = &self.cache {
                        let _ = cache.refresh(&final_url, false, etag.as_deref());
                    }
                    let rendered = parsed.render_paginated(req.topic.as_deref(), max_chars, offset);
                    return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                        "# {host} llms.txt (revalidated)\n\n{rendered}"
                    ))]));
                }
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "llms.txt not available for {host}: empty revalidated cache"
                ))]))
            }
            Ok(FetchOutcome::Fetched {
                body,
                etag,
                final_url: resolved_url,
            }) => {
                let parsed = ParsedDoc {
                    sections: parse_markdown_sections(&body, "", "", &host),
                    fallback: String::new(),
                    source_url: resolved_url.clone(),
                    lang: "markdown".to_string(),
                    metadata: Vec::new(),
                };
                if let Some(cache) = &self.cache {
                    if let Ok(serialized) = serde_json::to_string(&parsed) {
                        if let Err(err) =
                            cache.put_with_etag(&final_url, &serialized, false, etag.as_deref())
                        {
                            tracing::warn!("failed to write llms.txt cache entry: {err}");
                        }
                    }
                }
                let rendered = parsed.render_paginated(req.topic.as_deref(), max_chars, offset);
                let hint = if !want_full && resolved_url.ends_with("/llms.txt") {
                    "\n\nTip: pass full=true to fetch /llms-full.txt for complete documentation text."
                } else {
                    ""
                };
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "# {host} ({resolved_url})\n\n{rendered}{hint}"
                ))]))
            }
            Err(err) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "llms.txt not found for {host} ({final_url}): {err}"
            ))])),
        }
    }

    #[tool(
        name = "fetch_local_docs",
        description = "Search and read local documentation inside a private repository or directory: Markdown/text (.md, .mdx, .txt, .rst), Jupyter notebooks (.ipynb), Office/OpenDocument (.docx, .pptx, .xlsx, .odt, .odp, .ods), EPUB (.epub), PDF (.pdf), and images (.png, .jpg, .jpeg, .webp, .tif, .tiff, .gif) through on-device Tesseract OCR when built with OCR support. Enforces anti-zip-bomb, anti-XXE, image-size, secret-exclusion, and anti-symlink-escape guards with topic ranking and pagination."
    )]
    async fn fetch_local_docs(
        &self,
        params: Parameters<FetchLocalDocsRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = params.0;
        let trimmed_dir = req.project_dir.trim();
        if trimmed_dir.is_empty() || trimmed_dir.contains('\0') {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "Invalid project_dir: must be a non-empty directory path.",
            )]));
        }

        let max_chars = clamp_max_chars(req.max_chars);
        let offset = req.offset.unwrap_or(0);

        // The scan does blocking file I/O and OCR; keep it off the shared daemon's async workers.
        let root = PathBuf::from(trimmed_dir);
        let topic = req.topic.clone();
        let cache = self.cache.clone();
        let result = tokio::task::spawn_blocking(move || {
            search_local_docs(&root, topic.as_deref(), max_chars, offset, cache.as_ref())
        })
        .await
        .unwrap_or_else(|err| Err(format!("local documentation task failed: {err}")));

        match result {
            Ok(rendered) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "# Local Docs ({trimmed_dir})\n\n{rendered}"
            ))])),
            Err(err) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Local documentation lookup failed: {err}"
            ))])),
        }
    }

    #[tool(
        name = "manage_cache",
        description = "Inspect or maintain the local fast-docs disk cache. Supports actions: 'stats' (default), 'prune' (delete expired/corrupt entries), and 'clear' (delete all entries)."
    )]
    async fn manage_cache(
        &self,
        params: Parameters<ManageCacheRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(cache) = &self.cache else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "Local disk cache is unavailable (HOME environment variable not set).",
            )]));
        };

        let action = params
            .0
            .action
            .as_deref()
            .map_or("stats", str::trim)
            .to_ascii_lowercase();

        let text = match action.as_str() {
            "" | "stats" => {
                let s = cache.stats();
                format!(
                    "# fast-docs Cache Stats\n\
                     - Directory: `{}`\n\
                     - Entries: {} total ({} fresh, {} expired, {} corrupt)\n\
                     - Size: {} bytes\n\
                     - TTLs: mutable {}h · pinned {}h · search {}m",
                    s.root.display(),
                    s.total_entries,
                    s.fresh_entries,
                    s.expired_entries,
                    s.corrupt_entries,
                    s.total_bytes,
                    DEFAULT_CACHE_TTL.as_secs() / 3600,
                    PINNED_CACHE_TTL.as_secs() / 3600,
                    SEARCH_CACHE_TTL.as_secs() / 60,
                )
            }
            "prune" => {
                let removed = cache.prune_expired(SystemTime::now());
                let s = cache.stats();
                format!(
                    "# fast-docs Cache Pruned\n\
                     - Removed: {removed} expired/corrupt entries\n\
                     - Remaining: {} entries ({} bytes)",
                    s.total_entries, s.total_bytes,
                )
            }
            "clear" => {
                let removed = cache.clear();
                format!(
                    "# fast-docs Cache Cleared\n\
                     - Removed: {removed} entries from `{}`",
                    cache.stats().root.display()
                )
            }
            other => {
                format!("Invalid cache action '{other}': expected 'stats', 'prune', or 'clear'.")
            }
        };

        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "generated by rmcp::tool_handler macro"
)]
#[tool_handler(router = self.tool_router)]
impl rmcp::ServerHandler for FastDocsServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("fast-docs", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "fast-docs: blazing-fast documentation fetcher for AI coding assistants. Supports Rust (docs.rs), Python (pypi.org), Go (pkg.go.dev), npm/JS/TS (unpkg.com / registry.npmjs.org), and llms.txt documentation sites with topic filtering, ETag revalidation, and local caching.",
            )
    }
}
