use std::io;
use std::time::SystemTime;

use fast_docs::cache::DiskCache;
use fast_docs::http_server::{parse_cli_mode, run_streamable_http_server, RunMode};
use fast_docs::tools::FastDocsServer;
use rmcp::{transport::stdio, ServiceExt};
use tracing_subscriber::{fmt, EnvFilter};

fn prune_user_cache() {
    if let Some(cache) = DiskCache::default_user_cache() {
        let removed = cache.prune_expired(SystemTime::now());
        if removed > 0 {
            tracing::info!(removed, "pruned expired cache entries");
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .with_writer(io::stderr)
        .try_init();

    prune_user_cache();

    #[cfg(fast_docs_ocr)]
    fast_docs::ocr::warm_up_engine();

    let args: Vec<String> = std::env::args().collect();
    let env_bind = std::env::var("FAST_DOCS_BIND").ok();

    match parse_cli_mode(&args, env_bind.as_deref()) {
        RunMode::Http(bind_addr) => {
            run_streamable_http_server(&bind_addr)
                .await
                .map_err(io::Error::other)?;
        }
        RunMode::Stdio => {
            let service = FastDocsServer::new()
                .serve(stdio())
                .await
                .inspect_err(|err| tracing::error!("error starting MCP stdio server: {err}"))?;
            service.waiting().await?;
        }
    }

    Ok(())
}
