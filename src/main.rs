use std::io;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use fast_docs::cache::DiskCache;
use fast_docs::http_server::{run_streamable_http_server, DEFAULT_HTTP_ADDR};
use fast_docs::tools::FastDocsServer;
use rmcp::{transport::stdio, ServiceExt};
use tracing_subscriber::{fmt, EnvFilter};

/// Interval between background cache pruning passes in daemon mode.
const DAEMON_PRUNE_INTERVAL: Duration = Duration::from_hours(1);

enum RunMode {
    Stdio,
    Http(String),
}

fn parse_run_mode(args: &[String]) -> RunMode {
    let mut idx = 1;
    while idx < args.len() {
        let arg = args[idx].as_str();
        if arg == "--http" {
            let bind_addr = args
                .get(idx + 1)
                .filter(|next| !next.starts_with('-'))
                .cloned()
                .unwrap_or_else(|| DEFAULT_HTTP_ADDR.to_string());
            return RunMode::Http(bind_addr);
        }
        if let Some(bind_addr) = arg.strip_prefix("--http=") {
            if bind_addr.is_empty() {
                return RunMode::Http(DEFAULT_HTTP_ADDR.to_string());
            }
            return RunMode::Http(bind_addr.to_string());
        }
        idx += 1;
    }
    RunMode::Stdio
}

/// Returns `true` when the process runs as a daemon (for example under `launchd`),
/// where `stdin` is bound to `/dev/null` rather than an MCP client pipe.
fn is_daemon_context() -> bool {
    match std::fs::read_link("/dev/fd/0") {
        Ok(target) => target == Path::new("/dev/null"),
        Err(_) => {
            std::fs::metadata("/dev/stdin").is_ok_and(|meta| meta.file_type().is_char_device())
        }
    }
}

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

    let args: Vec<String> = std::env::args().collect();
    match parse_run_mode(&args) {
        RunMode::Http(bind_addr) => {
            run_streamable_http_server(&bind_addr)
                .await
                .map_err(io::Error::other)?;
            return Ok(());
        }
        RunMode::Stdio => {}
    }

    if is_daemon_context() {
        tracing::info!("fast-docs daemon mode: periodic cache maintenance only");
        loop {
            tokio::time::sleep(DAEMON_PRUNE_INTERVAL).await;
            prune_user_cache();
        }
    }

    let service = FastDocsServer::new()
        .serve(stdio())
        .await
        .inspect_err(|err| tracing::error!("error starting MCP server: {err}"))?;

    service.waiting().await?;
    Ok(())
}
