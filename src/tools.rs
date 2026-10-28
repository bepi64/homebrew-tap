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

const MAX_OUTPUT_CHARS: usize = 6500;
const USER_AGENT: &str = "fast-docs/0.1.0 (+https://github.com/bepi64/homebrew-tap)";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FetchDocsRequest {
    #[schemars(description = "Library name or module path (e.g. tokio, serde, flask, gin, github.com/gin-gonic/gin)")]
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
    let hash = Sha256::digest(format!("c7v2:{}", url).as_bytes());
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

/// Format HTML documentation into Context7-style structured snippets:
/// TITLE / DESCRIPTION / SOURCE / LANGUAGE / CODE separated by `----------------------------------------`.
fn html_to_context7(html: &str, source_url: &str, lang: &str, lib_name: &str) -> String {
    let doc = scraper::Html::parse_document(html);

    let container_selectors = [
        "div.docblock",                // docs.rs / rustdoc
        "div.UnitReadme-content",      // pkg.go.dev README
        "div.Documentation-content",   // pkg.go.dev API docs
        "div.project-description",     // pypi.org HTML fallback
        "article",
        "main",
    ];

    let mut sections: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
    let mut cur_title = format!("{} Overview", lib_name);
    let mut cur_desc: Vec<String> = Vec::new();
    let mut cur_code: Vec<String> = Vec::new();

    let children_selector = scraper::Selector::parse("h1, h2, h3, h4, p, li, pre").unwrap();

    let mut matched_container = false;
    for sel_str in &container_selectors {
        if let Ok(sel) = scraper::Selector::parse(sel_str) {
            let containers: Vec<_> = doc.select(&sel).collect();
            if containers.is_empty() {
                continue;
            }
            matched_container = true;
            for container in containers {
                for el in container.select(&children_selector) {
                    let tag = el.value().name();
                    match tag {
                        "h1" | "h2" | "h3" | "h4" => {
                            if !cur_desc.is_empty() || !cur_code.is_empty() {
                                sections.push((
                                    cur_title.clone(),
                                    std::mem::take(&mut cur_desc),
                                    std::mem::take(&mut cur_code),
                                ));
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
                            // Skip if inside a <pre>
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
    }

    if !cur_desc.is_empty() || !cur_code.is_empty() {
        sections.push((cur_title, cur_desc, cur_code));
    }

    if !matched_container || sections.is_empty() {
        let fallback = clean_inline_text(&doc.root_element());
        return truncate_smart(&fallback, MAX_OUTPUT_CHARS);
    }

    render_context7_sections(&sections, source_url, lang)
}

/// Convert raw Markdown (e.g. from PyPI JSON API) into Context7-style blocks.
fn markdown_to_context7(md: &str, summary: &str, source_url: &str, lang: &str, lib_name: &str) -> String {
    let mut sections: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
    let mut cur_title = format!("{} Overview", lib_name);
    let mut cur_desc: Vec<String> = Vec::new();
    if !summary.trim().is_empty() {
        cur_desc.push(summary.trim().to_string());
    }
    let mut cur_code: Vec<String> = Vec::new();

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
                sections.push((
                    cur_title.clone(),
                    std::mem::take(&mut cur_desc),
                    std::mem::take(&mut cur_code),
                ));
            }
            let heading = trimmed.trim_start_matches('#').trim().to_string();
            if !heading.is_empty() {
                cur_title = heading;
            }
        } else if !trimmed.is_empty() {
            // Skip lines that are purely HTML tags or image badges
            if trimmed.starts_with("[![")
                || trimmed.starts_with("<img")
                || trimmed.starts_with("<br")
                || (trimmed.starts_with('<') && trimmed.ends_with('>'))
            {
                continue;
            }
            // Strip inline HTML tags from mixed content lines
            let cleaned = strip_html_tags(trimmed);
            let cleaned = cleaned.trim();
            if !cleaned.is_empty() {
                cur_desc.push(cleaned.to_string());
            }
        }
    }

    if !cur_desc.is_empty() || !cur_code.is_empty() {
        sections.push((cur_title, cur_desc, cur_code));
    }

    render_context7_sections(&sections, source_url, lang)
}

fn render_context7_sections(
    sections: &[(String, Vec<String>, Vec<String>)],
    source_url: &str,
    lang: &str,
) -> String {
    let mut out = String::new();

    // Prioritize sections that have code examples, while keeping the first overview section
    let mut ordered: Vec<&(String, Vec<String>, Vec<String>)> = Vec::new();
    if let Some(first) = sections.first() {
        ordered.push(first);
    }
    for s in sections.iter().skip(1) {
        if !s.2.is_empty() {
            ordered.push(s);
        }
    }
    for s in sections.iter().skip(1) {
        if s.2.is_empty() && !s.1.is_empty() {
            ordered.push(s);
        }
    }

    for (title, desc, codes) in ordered {
        let mut block = String::new();
        block.push_str(&format!("TITLE: {}\n", title));
        if !desc.is_empty() {
            let desc_joined = desc.iter().take(4).cloned().collect::<Vec<_>>().join(" ");
            block.push_str(&format!("DESCRIPTION: {}\n", truncate_smart(&desc_joined, 450)));
        }
        block.push_str(&format!("SOURCE: {}\n", source_url));
        block.push_str(&format!("LANGUAGE: {}\n", lang));

        if !codes.is_empty() {
            block.push_str("CODE:\n");
            for code in codes.iter().take(2) {
                block.push_str(&format!("```{}\n{}\n```\n", lang, truncate_smart(code, 1200)));
            }
        }
        block.push_str("----------------------------------------\n\n");

        if out.len() + block.len() > MAX_OUTPUT_CHARS && !out.is_empty() {
            break;
        }
        out.push_str(&block);
    }

    truncate_smart(out.trim(), MAX_OUTPUT_CHARS)
}

fn strip_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => { in_tag = false; }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn clean_inline_text(el: &scraper::ElementRef) -> String {
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

fn truncate_smart(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let slice = &s[..end];
    let boundary = slice.rfind('\n').or_else(|| slice.rfind(' ')).unwrap_or(end);
    format!("{}\n... (truncated)", &slice[..boundary])
}

fn detect_lang(library: &str) -> &'static str {
    let lower = library.to_lowercase();
    if lower.contains("github.com/")
        || lower.contains("golang.org/")
        || lower.contains("gorm.io/")
        || lower.contains("go.uber.org/")
    {
        return "go";
    }

    let python_libs = [
        "flask", "django", "fastapi", "requests", "numpy", "pandas",
        "pytest", "pydantic", "sqlalchemy", "celery", "boto3", "pip",
        "httpx", "aiohttp", "uvicorn", "rich", "typer", "click",
    ];
    let go_libs = [
        "gin", "echo", "fiber", "cobra", "viper", "gorm",
        "zap", "logrus", "chi", "mux", "gorilla", "fx",
    ];

    if python_libs.iter().any(|l| lower == *l) {
        return "python";
    }
    if go_libs.iter().any(|l| lower == *l) {
        return "go";
    }
    "rust"
}

fn resolve_go_module(library: &str) -> String {
    match library.to_lowercase().as_str() {
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

fn build_url(library: &str, version: &str, lang: &str) -> String {
    match lang {
        "python" => {
            if version == "latest" || version.is_empty() {
                format!("https://pypi.org/pypi/{}/json", library)
            } else {
                format!("https://pypi.org/pypi/{}/{}/json", library, version)
            }
        }
        "go" => {
            let module = resolve_go_module(library);
            if version == "latest" || version.is_empty() {
                format!("https://pkg.go.dev/{}", module)
            } else {
                format!("https://pkg.go.dev/{}@{}", module, version)
            }
        }
        _ => format!("https://docs.rs/{}/{}", library, version),
    }
}

fn urlencoding(s: &str) -> String {
    let mut encoded = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{:02X}", b)),
        }
    }
    encoded
}

#[derive(Clone)]
pub struct FastDocsServer {
    tool_router: ToolRouter<Self>,
    client: reqwest::Client,
}

impl FastDocsServer {
    async fn fetch_url(&self, url: &str) -> Result<String, String> {
        let timeout = std::time::Duration::from_secs(10);
        match tokio::time::timeout(timeout, self.client.get(url).send()).await {
            Ok(Ok(resp)) if resp.status().is_success() => {
                resp.text().await.map_err(|e| e.to_string())
            }
            Ok(Ok(resp)) => Err(format!("HTTP {}", resp.status().as_u16())),
            Ok(Err(e)) => Err(format!("request failed: {}", e)),
            Err(_) => Err("request timeout".to_string()),
        }
    }
}

#[tool_router]
impl FastDocsServer {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            tool_router: Self::tool_router(),
            client,
        }
    }

    #[tool(name = "fetch_docs", description = "Fetch documentation and code examples for a library. Supports Rust (docs.rs), Python (pypi.org), and Go (pkg.go.dev). Returns Context7-style structured markdown with preserved code blocks.")]
    async fn fetch_docs(&self, params: Parameters<FetchDocsRequest>) -> Result<CallToolResult, rmcp::Error> {
        let req = params.0;
        let lang = req.lang.as_deref().unwrap_or_else(|| detect_lang(&req.library));
        let ver = req.version.unwrap_or_else(|| "latest".to_string());
        let url = build_url(&req.library, &ver, lang);

        // Check local disk cache first
        if let Some(cached) = read_cache(&url) {
            return Ok(CallToolResult::success(vec![Content::text(
                format!("# {} (cached)\n\n{}", req.library, cached)
            )]));
        }

        match self.fetch_url(&url).await {
            Ok(body) => {
                let formatted = if lang == "python" {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                        let info = &json["info"];
                        let summary = info["summary"].as_str().unwrap_or("");
                        let desc = info["description"].as_str().unwrap_or("");
                        let page_url = info["package_url"]
                            .as_str()
                            .unwrap_or("https://pypi.org");
                        markdown_to_context7(desc, summary, page_url, "python", &req.library)
                    } else {
                        html_to_context7(&body, &url, lang, &req.library)
                    }
                } else {
                    html_to_context7(&body, &url, lang, &req.library)
                };

                write_cache(&url, &formatted);
                Ok(CallToolResult::success(vec![Content::text(
                    format!("# {} ({})\n\n{}", req.library, lang, formatted)
                )]))
            }
            Err(e) => Ok(CallToolResult::success(vec![Content::text(
                format!("Documentation not found for {} ({}): {}", req.library, lang, e)
            )])),
        }
    }

    #[tool(name = "search_docs", description = "Search documentation across Rust (crates.io / docs.rs), Go (pkg.go.dev), and Python (PyPI / GitHub). Returns structured package identifiers and descriptions.")]
    async fn search_docs(&self, params: Parameters<SearchDocsRequest>) -> Result<CallToolResult, rmcp::Error> {
        let req = params.0;
        let lang = req.lang.as_deref().unwrap_or("all");
        let query = urlencoding(&req.query);

        let mut results = Vec::new();
        let mut errors = Vec::new();

        // 1. Search Rust (docs.rs)
        if lang == "all" || lang == "rust" {
            let url = format!("https://docs.rs/releases/search?query={}", query);
            match self.fetch_url(&url).await {
                Ok(html) => {
                    let doc = scraper::Html::parse_document(&html);
                    if let (Ok(rel_sel), Ok(name_sel), Ok(desc_sel)) = (
                        scraper::Selector::parse("a.release"),
                        scraper::Selector::parse("div.name"),
                        scraper::Selector::parse("div.description"),
                    ) {
                        let entries: Vec<String> = doc
                            .select(&rel_sel)
                            .take(8)
                            .filter_map(|el| {
                                let name = el.select(&name_sel).next().map(|n| clean_inline_text(&n))?;
                                let desc = el
                                    .select(&desc_sel)
                                    .next()
                                    .map(|d| clean_inline_text(&d))
                                    .unwrap_or_default();
                                Some(format!("- **{}** (rust): {}", name, desc))
                            })
                            .collect();
                        if !entries.is_empty() {
                            results.push(format!("## Rust (docs.rs)\n{}", entries.join("\n")));
                        }
                    }
                }
                Err(e) => errors.push(format!("rust: {}", e)),
            }
        }

        // 2. Search Python (GitHub Python packages API + direct PyPI lookup if single word)
        if lang == "all" || lang == "python" {
            let mut py_entries: Vec<String> = Vec::new();

            if !req.query.contains(' ') {
                let pypi_url = format!("https://pypi.org/pypi/{}/json", query);
                if let Ok(body) = self.fetch_url(&pypi_url).await {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                        let name = json["info"]["name"].as_str().unwrap_or(&req.query);
                        let ver = json["info"]["version"].as_str().unwrap_or("latest");
                        let sum = json["info"]["summary"].as_str().unwrap_or("");
                        py_entries.push(format!("- **{}-{}** (python / PyPI exact): {}", name, ver, sum));
                    }
                }
            }

            let gh_url = format!(
                "https://api.github.com/search/repositories?q={}+language:python&sort=stars&per_page=6",
                query
            );
            match self.fetch_url(&gh_url).await {
                Ok(body) => {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                        if let Some(items) = json["items"].as_array() {
                            for item in items.iter().take(6) {
                                let name = item["name"].as_str().unwrap_or("");
                                let desc = item["description"].as_str().unwrap_or("");
                                if !name.is_empty() {
                                    py_entries.push(format!("- **{}** (python): {}", name, desc));
                                }
                            }
                        }
                    }
                }
                Err(e) => errors.push(format!("python: {}", e)),
            }

            if !py_entries.is_empty() {
                results.push(format!("## Python (PyPI)\n{}", py_entries.join("\n")));
            }
        }

        // 3. Search Go (pkg.go.dev)
        if lang == "all" || lang == "go" {
            let url = format!("https://pkg.go.dev/search?q={}", query);
            match self.fetch_url(&url).await {
                Ok(html) => {
                    let doc = scraper::Html::parse_document(&html);
                    if let (Ok(snip_sel), Ok(a_sel), Ok(syn_sel)) = (
                        scraper::Selector::parse("div.SearchSnippet"),
                        scraper::Selector::parse("a"),
                        scraper::Selector::parse("p.SearchSnippet-synopsis"),
                    ) {
                        let entries: Vec<String> = doc
                            .select(&snip_sel)
                            .take(8)
                            .filter_map(|el| {
                                let header = el.select(&a_sel).next().map(|a| clean_inline_text(&a))?;
                                let syn = el
                                    .select(&syn_sel)
                                    .next()
                                    .map(|p| clean_inline_text(&p))
                                    .unwrap_or_default();
                                Some(format!("- **{}** (go): {}", header, syn))
                            })
                            .collect();
                        if !entries.is_empty() {
                            results.push(format!("## Go (pkg.go.dev)\n{}", entries.join("\n")));
                        }
                    }
                }
                Err(e) => errors.push(format!("go: {}", e)),
            }
        }

        if results.is_empty() {
            if !errors.is_empty() {
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "No results found for '{}' (errors: {})",
                    req.query,
                    errors.join(", ")
                ))]))
            } else {
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "No results found for: {}",
                    req.query
                ))]))
            }
        } else {
            Ok(CallToolResult::success(vec![Content::text(format!(
                "# Search: {}\n\n{}",
                req.query,
                results.join("\n\n")
            ))]))
        }
    }
}

#[tool_handler]
impl rmcp::ServerHandler for FastDocsServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some("fast-docs: blazing-fast documentation fetcher for AI coding assistants. Supports Rust (docs.rs), Python (pypi.org), and Go (pkg.go.dev). Returns Context7-style structured markdown with preserved code blocks and local caching.".into()),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
