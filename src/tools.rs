use std::future::Future;
use std::path::PathBuf;
use rmcp::{
    tool, tool_router, tool_handler,
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo},
    handler::server::router::tool::ToolRouter,
    handler::server::tool::Parameters,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FetchDocsRequest {
    #[schemars(description = "Library name (e.g. tokio, reqwest, serde, flask, gin)")]
    pub library: String,
    #[schemars(description = "Optional version (default: latest)")]
    pub version: Option<String>,
    #[schemars(description = "Language ecosystem: rust, python, or go (default: auto-detect)")]
    pub lang: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchDocsRequest {
    #[schemars(description = "Search query for documentation")]
    pub query: String,
    #[schemars(description = "Language ecosystem to search: rust, python, go, or all (default: all)")]
    pub lang: Option<String>,
}

fn cache_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".cache").join("fast-docs").join("indexes"))
}

fn cache_key(url: &str) -> String {
    use sha2::{Sha256, Digest};
    let hash = Sha256::digest(url.as_bytes());
    hex::encode(&hash[..8])
}

fn read_cache(url: &str) -> Option<String> {
    let dir = cache_dir()?;
    let path = dir.join(cache_key(url));
    std::fs::read_to_string(path).ok()
}

fn write_cache(url: &str, content: &str) {
    if let Some(dir) = cache_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(cache_key(url));
        let _ = std::fs::write(path, content);
    }
}

fn html_to_markdown(html: &str) -> String {
    let doc = scraper::Html::parse_document(html);

    // Try to extract the main documentation block (rustdoc, pydoc, godoc)
    let selectors = [
        "div.docblock",           // docs.rs / rustdoc
        "section.Documentation",  // pkg.go.dev
        "div.project-description",// pypi.org
        "article",                // fallback
        "main",                   // fallback
    ];

    for sel in &selectors {
        if let Ok(selector) = scraper::Selector::parse(sel) {
            let blocks: Vec<String> = doc.select(&selector)
                .map(|el| extract_text(&el))
                .collect();
            if !blocks.is_empty() {
                let text = blocks.join("\n\n");
                if text.len() > 100 {
                    return truncate(&text, 8000);
                }
            }
        }
    }

    // Last resort: extract all text
    let text = doc.root_element().text().collect::<Vec<_>>().join(" ");
    truncate(&text, 8000)
}

fn extract_text(el: &scraper::ElementRef) -> String {
    let mut out = String::new();
    for node in el.text() {
        let trimmed = node.trim();
        if !trimmed.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(trimmed);
        }
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let boundary = s[..max].rfind(' ').unwrap_or(max);
        format!("{}... (truncated)", &s[..boundary])
    }
}

fn detect_lang(library: &str) -> &'static str {
    let python_libs = ["flask", "django", "fastapi", "requests", "numpy", "pandas",
                       "pytest", "pydantic", "sqlalchemy", "celery", "boto3", "pip"];
    let go_libs = ["gin", "echo", "fiber", "cobra", "viper", "gorm",
                   "zap", "logrus", "chi", "mux", "gorilla"];

    let lower = library.to_lowercase();
    if python_libs.iter().any(|l| lower == *l) { return "python"; }
    if go_libs.iter().any(|l| lower == *l) { return "go"; }
    "rust"
}

fn build_url(library: &str, version: &str, lang: &str) -> String {
    match lang {
        "python" => format!("https://pypi.org/project/{}/", library),
        "go" => format!("https://pkg.go.dev/{}", library),
        _ => format!("https://docs.rs/{}/{}", library, version),
    }
}

fn urlencoding(s: &str) -> String {
    s.replace(' ', "+")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('#', "%23")
}

async fn fetch_url(url: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| e.to_string())?;

    let timeout = std::time::Duration::from_secs(10);
    match tokio::time::timeout(timeout, client.get(url).send()).await {
        Ok(Ok(resp)) if resp.status().is_success() => {
            resp.text().await.map_err(|e| e.to_string())
        }
        Ok(Ok(resp)) => Err(format!("HTTP {}", resp.status().as_u16())),
        Ok(Err(e)) => Err(format!("request failed: {}", e)),
        Err(_) => Err("request timeout".to_string()),
    }
}

#[derive(Clone)]
pub struct FastDocsServer {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl FastDocsServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(name = "fetch_docs", description = "Fetch documentation for a library. Supports Rust (docs.rs), Python (pypi.org), and Go (pkg.go.dev). Returns clean markdown.")]
    async fn fetch_docs(&self, params: Parameters<FetchDocsRequest>) -> Result<CallToolResult, rmcp::Error> {
        let req = params.0;
        let lang = req.lang.as_deref().unwrap_or_else(|| detect_lang(&req.library));
        let ver = req.version.unwrap_or_else(|| "latest".to_string());
        let url = build_url(&req.library, &ver, lang);

        // Check cache first
        if let Some(cached) = read_cache(&url) {
            return Ok(CallToolResult::success(vec![Content::text(
                format!("# {} (cached)\n\n{}", req.library, cached)
            )]));
        }

        match fetch_url(&url).await {
            Ok(html) => {
                let md = html_to_markdown(&html);
                write_cache(&url, &md);
                Ok(CallToolResult::success(vec![Content::text(
                    format!("# {} — {}\nSource: {}\n\n{}", req.library, lang, url, md)
                )]))
            }
            Err(e) => Ok(CallToolResult::success(vec![Content::text(
                format!("Documentation not found for {} ({}): {}", req.library, lang, e)
            )])),
        }
    }

    #[tool(name = "search_docs", description = "Search documentation across docs.rs (Rust), pkg.go.dev (Go), and pypi.org (Python). Returns a list of matching packages.")]
    async fn search_docs(&self, params: Parameters<SearchDocsRequest>) -> Result<CallToolResult, rmcp::Error> {
        let req = params.0;
        let lang = req.lang.as_deref().unwrap_or("all");
        let query = urlencoding(&req.query);

        let mut results = Vec::new();

        // Search docs.rs (Rust)
        if lang == "all" || lang == "rust" {
            let url = format!("https://docs.rs/releases/search?query={}", query);
            if let Ok(html) = fetch_url(&url).await {
                let doc = scraper::Html::parse_document(&html);
                if let Ok(sel) = scraper::Selector::parse("a.release div.description") {
                    let mut entries: Vec<String> = doc.select(&sel)
                        .take(10)
                        .map(|el| format!("  - {}", extract_text(&el)))
                        .collect();
                    if !entries.is_empty() {
                        results.push(format!("## Rust (docs.rs)\n{}", entries.join("\n")));
                    }
                }
            }
        }

        // Search pypi.org (Python)
        if lang == "all" || lang == "python" {
            let url = format!("https://pypi.org/search/?q={}", query);
            if let Ok(html) = fetch_url(&url).await {
                let doc = scraper::Html::parse_document(&html);
                if let Ok(sel) = scraper::Selector::parse("a.package-snippet h3") {
                    let entries: Vec<String> = doc.select(&sel)
                        .take(10)
                        .map(|el| format!("  - {}", extract_text(&el)))
                        .collect();
                    if !entries.is_empty() {
                        results.push(format!("## Python (pypi.org)\n{}", entries.join("\n")));
                    }
                }
            }
        }

        // Search pkg.go.dev (Go)
        if lang == "all" || lang == "go" {
            let url = format!("https://pkg.go.dev/search?q={}", query);
            if let Ok(html) = fetch_url(&url).await {
                let doc = scraper::Html::parse_document(&html);
                if let Ok(sel) = scraper::Selector::parse("div.SearchSnippet a") {
                    let entries: Vec<String> = doc.select(&sel)
                        .take(10)
                        .map(|el| format!("  - {}", extract_text(&el)))
                        .collect();
                    if !entries.is_empty() {
                        results.push(format!("## Go (pkg.go.dev)\n{}", entries.join("\n")));
                    }
                }
            }
        }

        if results.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                format!("No results found for: {}", req.query)
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(
                format!("# Search: {}\n\n{}", req.query, results.join("\n\n"))
            )]))
        }
    }
}

#[tool_handler]
impl rmcp::ServerHandler for FastDocsServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some("fast-docs: blazing-fast documentation fetcher for AI coding assistants. Supports Rust (docs.rs), Python (pypi.org), and Go (pkg.go.dev). Returns clean markdown with local caching.".into()),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
