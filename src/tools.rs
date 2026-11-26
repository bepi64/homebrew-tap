use std::fmt::Write as _;
use std::path::Path;
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
use crate::llms_txt::{build_llms_txt_url, build_pinned_llms_client, resolve_public_socket_addrs};
use crate::local_docs::search_local_docs;
use crate::manifest::resolve_project_version;

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

/// Scope selector for `search_docs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchScope {
    All,
    Only(Ecosystem),
}

impl SearchScope {
    fn from_opt(raw: Option<&str>) -> Self {
        let Some(value) = raw else {
            return Self::All;
        };
        if value.trim().eq_ignore_ascii_case("all") {
            return Self::All;
        }
        Ecosystem::parse(value).map_or(Self::All, Self::Only)
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
        description = "Optional local project directory path used to auto-resolve the library version from Cargo.lock, Cargo.toml, package.json, pyproject.toml, or go.mod when version is omitted"
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

/// Detect the most likely documentation ecosystem for a library identifier.
#[must_use]
pub fn detect_lang(library: &str) -> Ecosystem {
    let lower = library.to_ascii_lowercase();
    if lower.contains("github.com/")
        || lower.contains("golang.org/")
        || lower.contains("gorm.io/")
        || lower.contains("go.uber.org/")
    {
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
    /// Upstream returned a `2xx` body and an optional `ETag`.
    Fetched { body: String, etag: Option<String> },
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
        Ok(FetchOutcome::Fetched { body, etag })
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
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParsedDoc {
    sections: Vec<DocSection>,
    fallback: String,
    source_url: String,
    lang: String,
}

impl ParsedDoc {
    fn render(&self, topic: Option<&str>, max_chars: usize) -> String {
        self.render_paginated(topic, max_chars, 0)
    }

    fn render_paginated(&self, topic: Option<&str>, max_chars: usize, offset: usize) -> String {
        if self.sections.is_empty() {
            if offset > 0 {
                return slice_with_pagination(&self.fallback, offset, max_chars);
            }
            return truncate_smart(&self.fallback, max_chars);
        }
        if offset > 0 {
            return render_doc_sections_paginated(
                &self.sections,
                &self.source_url,
                &self.lang,
                topic,
                max_chars,
                offset,
            );
        }
        render_context7_sections(
            &self.sections,
            &self.source_url,
            &self.lang,
            topic,
            max_chars,
        )
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

fn parse_html_doc(html: &str, source_url: &str, ecosystem: Ecosystem, lib_name: &str) -> ParsedDoc {
    let doc = scraper::Html::parse_document(html);

    let container_selectors = [
        "div.docblock",              // docs.rs / rustdoc
        "div.UnitReadme-content",    // pkg.go.dev README
        "div.Documentation-content", // pkg.go.dev API docs
        "div.project-description",   // pypi.org HTML fallback
        "article",
        "main",
    ];

    let Ok(children_selector) = scraper::Selector::parse("h1, h2, h3, h4, p, li, pre") else {
        return ParsedDoc {
            sections: Vec::new(),
            fallback: clean_inline_text(&doc.root_element()),
            source_url: source_url.to_string(),
            lang: ecosystem.as_str().to_string(),
        };
    };

    let mut sections: Vec<DocSection> = Vec::new();
    let mut cur_title = format!("{lib_name} Overview");
    let mut cur_desc: Vec<String> = Vec::new();
    let mut cur_code: Vec<String> = Vec::new();
    let mut matched_container = false;

    for sel_str in &container_selectors {
        let Ok(sel) = scraper::Selector::parse(sel_str) else {
            continue;
        };
        let containers: Vec<_> = doc.select(&sel).collect();
        if containers.is_empty() {
            continue;
        }
        matched_container = true;
        for container in containers {
            for el in container.select(&children_selector) {
                match el.value().name() {
                    "h1" | "h2" | "h3" | "h4" => {
                        if !cur_desc.is_empty() || !cur_code.is_empty() {
                            sections.push(DocSection {
                                title: cur_title.clone(),
                                descriptions: std::mem::take(&mut cur_desc),
                                codes: std::mem::take(&mut cur_code),
                                source: None,
                            });
                        }
                        let heading = clean_inline_text(&el)
                            .trim_start_matches('§')
                            .trim()
                            .to_string();
                        if !heading.is_empty() {
                            cur_title = heading;
                        }
                    }
                    "pre" => {
                        let code = el.text().collect::<String>().trim().to_string();
                        if !code.is_empty() {
                            cur_code.push(code);
                        }
                    }
                    "p" | "li" => {
                        let txt = clean_inline_text(&el);
                        if txt.len() > 15 && !cur_desc.contains(&txt) {
                            cur_desc.push(txt);
                        }
                    }
                    _ => {}
                }
            }
        }
        if !sections.is_empty() || !cur_desc.is_empty() || !cur_code.is_empty() {
            break;
        }
    }

    if !cur_desc.is_empty() || !cur_code.is_empty() {
        sections.push(DocSection {
            title: cur_title,
            descriptions: cur_desc,
            codes: cur_code,
            source: None,
        });
    }

    let fallback = if !matched_container || sections.is_empty() {
        clean_inline_text(&doc.root_element())
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
    }
}

pub(crate) fn parse_markdown_sections(
    md: &str,
    summary: &str,
    source_label: &str,
    lib_name: &str,
) -> Vec<DocSection> {
    let mut sections: Vec<DocSection> = Vec::new();
    let mut cur_title = format!("{lib_name} Overview");
    let mut cur_desc: Vec<String> = Vec::new();
    if !summary.trim().is_empty() {
        cur_desc.push(summary.trim().to_string());
    }
    let mut cur_code: Vec<String> = Vec::new();
    let source_opt = (!source_label.is_empty()).then(|| source_label.to_string());

    let mut in_code_block = false;
    let mut code_buf: Vec<String> = Vec::new();

    for line in md.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if in_code_block {
                let block = code_buf.join("\n").trim().to_string();
                if !block.is_empty() {
                    cur_code.push(block);
                }
                code_buf.clear();
                in_code_block = false;
            } else {
                in_code_block = true;
            }
            continue;
        }

        if in_code_block {
            code_buf.push(line.to_string());
            continue;
        }

        if trimmed.starts_with('#') {
            if !cur_desc.is_empty() || !cur_code.is_empty() {
                sections.push(DocSection {
                    title: cur_title.clone(),
                    descriptions: std::mem::take(&mut cur_desc),
                    codes: std::mem::take(&mut cur_code),
                    source: source_opt.clone(),
                });
            }
            let heading = trimmed.trim_start_matches('#').trim().to_string();
            if !heading.is_empty() {
                cur_title = heading;
            }
        } else if !trimmed.is_empty() {
            if trimmed.starts_with("[![")
                || trimmed.starts_with("<img")
                || trimmed.starts_with("<br")
                || (trimmed.starts_with('<') && trimmed.ends_with('>'))
            {
                continue;
            }
            let cleaned = strip_html_tags(trimmed);
            let cleaned = cleaned.trim();
            if !cleaned.is_empty() {
                cur_desc.push(cleaned.to_string());
            }
        }
    }

    if !cur_desc.is_empty() || !cur_code.is_empty() {
        sections.push(DocSection {
            title: cur_title,
            descriptions: cur_desc,
            codes: cur_code,
            source: source_opt,
        });
    }

    sections
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

fn tokenize_topic(topic: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for raw in topic.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_') {
        let token = raw.to_ascii_lowercase();
        if !token.is_empty() && !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    tokens
}

fn score_section(section: &DocSection, tokens: &[String]) -> usize {
    let title_lower = section.title.to_ascii_lowercase();
    let desc_lower = section.descriptions.join(" ").to_ascii_lowercase();
    let code_lower = section.codes.join("\n").to_ascii_lowercase();

    let mut score = 0_usize;
    for token in tokens {
        if title_lower.contains(token) {
            score += 10;
        }
        if code_lower.contains(token) {
            score += 4;
        }
        if desc_lower.contains(token) {
            score += 2;
        }
    }

    if score > 0 && !section.codes.is_empty() {
        score += 3;
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
            if let Some(raw_topic) = topic {
                let _ = writeln!(
                    out,
                    "Note: No sections matched topic '{}'; showing overview sections instead.\n",
                    raw_topic.trim()
                );
            }
            default_section_order(sections)
        } else {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            scored.into_iter().map(|(_, _, section)| section).collect()
        }
    };

    for section in ordered {
        let mut block = String::new();
        let _ = writeln!(block, "TITLE: {}", section.title);
        if !section.descriptions.is_empty() {
            let desc_joined = section
                .descriptions
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            let _ = writeln!(block, "DESCRIPTION: {}", truncate_smart(&desc_joined, 450));
        }
        let src = section.source.as_deref().unwrap_or(default_source);
        let _ = writeln!(block, "SOURCE: {src}");
        let _ = writeln!(block, "LANGUAGE: {lang}");

        if !section.codes.is_empty() {
            block.push_str("CODE:\n");
            for code in section.codes.iter().take(2) {
                let _ = writeln!(block, "```{lang}\n{}\n```", truncate_smart(code, 1200));
            }
        }
        block.push_str("----------------------------------------\n\n");

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

fn render_context7_sections(
    sections: &[DocSection],
    source_url: &str,
    lang: &str,
    topic: Option<&str>,
    max_chars: usize,
) -> String {
    let rendered = format_sections_all(sections, source_url, lang, topic, max_chars);
    truncate_smart(&rendered, max_chars)
}

/// Remove inline HTML tag markup from a text line.
#[must_use]
pub fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
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
            let name = clean_inline_text(&name_el);
            let desc = el
                .select(&desc_sel)
                .next()
                .map_or_default(|d| clean_inline_text(&d));
            entries.push(format!("- **{name}** (rust): {desc}"));
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
            let Ok(exact_url) = build_url(raw_query, "latest", Ecosystem::Python) else {
                return None;
            };
            let body = self.fetch_allowlisted(&exact_url).await.ok()?;
            let json = serde_json::from_str::<serde_json::Value>(&body).ok()?;
            let name = json["info"]["name"].as_str().unwrap_or(raw_query);
            let ver = json["info"]["version"].as_str().unwrap_or("latest");
            let sum = json["info"]["summary"].as_str().unwrap_or("");
            Some(format!("- **{name}-{ver}** (python / PyPI exact): {sum}"))
        };

        let gh_url = format!(
            "https://api.github.com/search/repositories?q={encoded_query}+language:python&sort=stars&per_page=6"
        );
        let gh_fut = self.fetch_allowlisted(&gh_url);

        let (exact_entry, gh_res) = tokio::join!(exact_fut, gh_fut);

        let mut py_entries: Vec<String> = Vec::new();
        if let Some(exact) = exact_entry {
            py_entries.push(exact);
        }

        match gh_res {
            Ok(body) => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                    if let Some(items) = json["items"].as_array() {
                        for item in items.iter().take(6) {
                            let name = item["name"].as_str().unwrap_or("");
                            let desc = item["description"].as_str().unwrap_or("");
                            if !name.is_empty() {
                                py_entries.push(format!("- **{name}** (python): {desc}"));
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
            Ok(Some(format!("## Python (PyPI)\n{}", py_entries.join("\n"))))
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
            entries.push(format!("- **{header}** (go): {syn}"));
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
        if let Ok(FetchOutcome::Fetched { body, etag }) =
            self.fetch_allowlisted_conditional(&lower_url, None).await
        {
            return Ok((
                parse_markdown_doc(&body, "", &lower_url, Ecosystem::Npm, library),
                etag,
            ));
        }

        let encoded_pkg = library.replace('/', "%2F");
        let registry_url = format!("https://registry.npmjs.org/{encoded_pkg}");
        let FetchOutcome::Fetched { body, etag } = self
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
            .unwrap_or(&registry_url);
        Ok((
            parse_markdown_doc(readme, summary, page_url, Ecosystem::Npm, library),
            etag,
        ))
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
        let ecosystem = req
            .lang
            .as_deref()
            .and_then(Ecosystem::parse)
            .unwrap_or_else(|| detect_lang(&req.library));

        let mut resolved_source: Option<&'static str> = None;
        let version = match req
            .version
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            Some(explicit) => explicit.to_string(),
            None => {
                if let Some(dir) = req
                    .project_dir
                    .as_deref()
                    .map(str::trim)
                    .filter(|d| !d.is_empty() && !d.contains('\0'))
                {
                    if let Some(resolved) =
                        resolve_project_version(Path::new(dir), &req.library, ecosystem)
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

        let url =
            match build_url_with_module(&req.library, &version, ecosystem, req.module.as_deref()) {
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
                        "# {label} (cached)\n\n{rendered}"
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
            Ok(FetchOutcome::Fetched { body, etag }) => {
                let parsed = match ecosystem {
                    Ecosystem::Python => {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                            let info = &json["info"];
                            let summary = info["summary"].as_str().unwrap_or("");
                            let desc = info["description"].as_str().unwrap_or("");
                            let page_url =
                                info["package_url"].as_str().unwrap_or("https://pypi.org");
                            parse_markdown_doc(
                                desc,
                                summary,
                                page_url,
                                Ecosystem::Python,
                                &req.library,
                            )
                        } else {
                            parse_html_doc(&body, &url, ecosystem, &req.library)
                        }
                    }
                    Ecosystem::Npm => {
                        parse_markdown_doc(&body, "", &url, Ecosystem::Npm, &req.library)
                    }
                    Ecosystem::Rust | Ecosystem::Go => {
                        parse_html_doc(&body, &url, ecosystem, &req.library)
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
            Err(err) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Documentation not found for {label} ({}): {err}",
                ecosystem.as_str()
            ))])),
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

        let scope = SearchScope::from_opt(req.lang.as_deref());
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

        let addrs = match resolve_public_socket_addrs(&host).await {
            Ok(addrs) => addrs,
            Err(err) => {
                return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Blocked or unreachable llms.txt host '{host}': {err}"
                ))]));
            }
        };

        let pinned_client =
            match build_pinned_llms_client(&host, &addrs, USER_AGENT, CONNECT_TIMEOUT) {
                Ok(client) => client,
                Err(err) => {
                    return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                        "Failed to initialize HTTP client for '{host}': {err}"
                    ))]));
                }
            };

        let outcome = fetch_url_conditional(
            &pinned_client,
            &url_str,
            stale_etag.as_deref(),
            REQUEST_TIMEOUT,
            MAX_RESPONSE_BYTES,
        )
        .await;

        let (final_url, final_outcome) = match outcome {
            Err(err) if want_full && err == "HTTP 404" => {
                if let Ok(fallback_url) = build_llms_txt_url(&req.target, false) {
                    let fb_str = fallback_url.as_str().to_string();
                    let fb_res = fetch_url_conditional(
                        &pinned_client,
                        &fb_str,
                        None,
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
            Ok(FetchOutcome::Fetched { body, etag }) => {
                let parsed = ParsedDoc {
                    sections: parse_markdown_sections(&body, "", "", &host),
                    fallback: String::new(),
                    source_url: final_url.clone(),
                    lang: "markdown".to_string(),
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
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "# {host} ({final_url})\n\n{rendered}"
                ))]))
            }
            Err(err) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "llms.txt not found for {host} ({final_url}): {err}"
            ))])),
        }
    }

    #[tool(
        name = "fetch_local_docs",
        description = "Search and read local documentation inside a private repository or directory: Markdown/text (.md, .mdx, .txt, .rst), Jupyter notebooks (.ipynb), Office/OpenDocument (.docx, .pptx, .xlsx, .odt, .odp, .ods), EPUB (.epub), and PDF (.pdf). Enforces anti-zip-bomb, anti-XXE, secret-exclusion, and anti-symlink-escape guards with topic ranking and pagination."
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

        match search_local_docs(
            Path::new(trimmed_dir),
            req.topic.as_deref(),
            max_chars,
            offset,
        ) {
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
