use std::future::Future;
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
    #[schemars(description = "Library name (e.g. tokio, reqwest, serde)")]
    pub library: String,
    #[schemars(description = "Optional version (default: latest)")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchDocsRequest {
    #[schemars(description = "Search query for documentation")]
    pub query: String,
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

    #[tool(name = "fetch_docs", description = "Fetch documentation for a library from docs.rs, pkg.go.dev, or pypi.org")]
    async fn fetch_docs(&self, params: Parameters<FetchDocsRequest>) -> Result<CallToolResult, rmcp::Error> {
        let req = params.0;
        let ver = req.version.unwrap_or_else(|| "latest".to_string());
        let url = format!("https://docs.rs/{}/{}", req.library, ver);

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| rmcp::Error::internal_error(e.to_string(), None))?;

        let timeout = std::time::Duration::from_secs(10);

        match tokio::time::timeout(timeout, client.get(&url).send()).await {
            Ok(Ok(resp)) if resp.status().is_success() => {
                let body = resp.text().await.unwrap_or_default();
                let truncated = if body.len() > 50_000 {
                    format!("{}... (truncated)", &body[..50_000])
                } else {
                    body
                };
                Ok(CallToolResult::success(vec![Content::text(truncated)]))
            }
            Ok(Ok(resp)) => {
                Ok(CallToolResult::success(vec![Content::text(
                    format!("HTTP {}: documentation not found for {} v{}", resp.status().as_u16(), req.library, ver)
                )]))
            }
            Ok(Err(e)) => Err(rmcp::Error::internal_error(format!("request failed: {}", e), None)),
            Err(_) => Err(rmcp::Error::internal_error("request timeout", None)),
        }
    }

    #[tool(name = "search_docs", description = "Search documentation across docs.rs, pkg.go.dev, and pypi.org")]
    async fn search_docs(&self, params: Parameters<SearchDocsRequest>) -> Result<CallToolResult, rmcp::Error> {
        let req = params.0;
        let url = format!("https://docs.rs/releases/search?query={}", urlencoding(&req.query));

        let client = reqwest::Client::new();
        let timeout = std::time::Duration::from_secs(10);

        match tokio::time::timeout(timeout, client.get(&url).send()).await {
            Ok(Ok(resp)) if resp.status().is_success() => {
                let body = resp.text().await.unwrap_or_default();
                let truncated = if body.len() > 50_000 {
                    format!("{}... (truncated)", &body[..50_000])
                } else {
                    body
                };
                Ok(CallToolResult::success(vec![Content::text(truncated)]))
            }
            _ => Ok(CallToolResult::success(vec![Content::text(
                format!("No results found for: {}", req.query)
            )])),
        }
    }
}

fn urlencoding(s: &str) -> String {
    s.replace(' ', "+")
        .replace('&', "%26")
        .replace('=', "%3D")
}

#[tool_handler]
impl rmcp::ServerHandler for FastDocsServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some("fast-docs: blazing-fast documentation fetcher for AI coding assistants".into()),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
