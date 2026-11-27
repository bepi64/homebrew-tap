use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use http::{header, Method, Response, StatusCode};
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::cache::DiskCache;
use crate::tools::FastDocsServer;

/// Default loopback bind address for the background MCP HTTP daemon.
pub const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:3000";

/// Interval between background cache pruning passes while the daemon runs.
const DAEMON_PRUNE_INTERVAL: Duration = Duration::from_hours(1);

/// Execution transport mode selected from CLI arguments and environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunMode {
    /// Persistent background Streamable HTTP daemon (default).
    Http(String),
    /// Per-client stdio JSON-RPC mode (`--stdio`).
    Stdio,
}

/// Resolve the server transport mode from CLI arguments and optional `FAST_DOCS_BIND` env var.
///
/// Defaults to `RunMode::Http` (shared background daemon) unless `--stdio` is explicitly passed.
#[must_use]
pub fn parse_cli_mode(args: &[String], env_bind: Option<&str>) -> RunMode {
    let default_bind = env_bind
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_HTTP_ADDR);

    let mut idx = 1;
    while idx < args.len() {
        let arg = args[idx].as_str();
        if arg == "--stdio" {
            return RunMode::Stdio;
        }
        if arg == "--http" {
            let bind_addr = args
                .get(idx + 1)
                .filter(|next| !next.starts_with('-'))
                .map_or(default_bind, String::as_str);
            return RunMode::Http(bind_addr.to_string());
        }
        if let Some(bind_addr) = arg.strip_prefix("--http=") {
            let trimmed = bind_addr.trim();
            let chosen = if trimmed.is_empty() {
                default_bind
            } else {
                trimmed
            };
            return RunMode::Http(chosen.to_string());
        }
        idx += 1;
    }

    RunMode::Http(default_bind.to_string())
}

/// Parse a bind address and verify it targets a loopback interface (`127.0.0.0/8` or `::1`)
/// to prevent accidental exposure on LAN/public interfaces.
///
/// # Errors
/// Returns an error if `raw` is not a valid `SocketAddr` or is not a loopback address.
pub fn parse_loopback_bind_addr(raw: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = raw
        .trim()
        .parse()
        .map_err(|err| format!("invalid bind address '{raw}': {err}"))?;
    if !addr.ip().is_loopback() {
        return Err(format!(
            "refusing non-loopback bind address '{addr}': bind to 127.0.0.1 or [::1]"
        ));
    }
    Ok(addr)
}

fn json_response(status: StatusCode, body: &str) -> Response<BoxBody<Bytes, Infallible>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.to_string())).boxed())
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()).boxed()))
}

/// Spawn the shared MCP Streamable HTTP daemon on a loopback address (`POST /mcp` + `GET /health`)
/// with a shared `FastDocsServer` HTTP/2 connection pool and `rmcp`'s built-in DNS rebinding guard.
///
/// # Errors
/// Returns an error if `bind_addr` is rejected or binding the TCP listener fails.
pub async fn spawn_streamable_http_server(
    bind_addr: &str,
) -> Result<(SocketAddr, oneshot::Sender<()>), String> {
    let parsed = parse_loopback_bind_addr(bind_addr)?;
    let listener = TcpListener::bind(parsed)
        .await
        .map_err(|err| format!("failed to bind {parsed}: {err}"))?;
    let local_addr = listener
        .local_addr()
        .map_err(|err| format!("failed to inspect local address: {err}"))?;

    // Share one FastDocsServer (and reqwest TLS/HTTP2 connection pool) across all concurrent agents.
    let shared_server = FastDocsServer::new();
    let service = Arc::new(StreamableHttpService::new(
        move || Ok(shared_server.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    ));

    let (tx, mut rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut rx => break,
                accepted = listener.accept() => {
                    let Ok((stream, _peer)) = accepted else {
                        continue;
                    };
                    let svc = Arc::clone(&service);
                    tokio::spawn(async move {
                        let io = TokioIo::new(stream);
                        let conn_svc = service_fn(move |req| {
                            let svc = Arc::clone(&svc);
                            async move {
                                let path = req.uri().path();
                                if path == "/health" && req.method() == Method::GET {
                                    let payload = format!(
                                        r#"{{"status":"ok","service":"fast-docs","version":"{}","ocr":{}}}"#,
                                        env!("CARGO_PKG_VERSION"),
                                        crate::ocr::health_json()
                                    );
                                    return Ok::<_, Infallible>(json_response(StatusCode::OK, &payload));
                                }
                                if path == "/mcp" {
                                    return Ok::<_, Infallible>(svc.handle(req).await);
                                }
                                Ok::<_, Infallible>(json_response(
                                    StatusCode::NOT_FOUND,
                                    r#"{"error":"not found; use /mcp or /health"}"#,
                                ))
                            }
                        });
                        if let Err(err) = http1::Builder::new()
                            .serve_connection(io, conn_svc)
                            .await
                        {
                            tracing::debug!("HTTP connection ended: {err}");
                        }
                    });
                }
            }
        }
    });

    Ok((local_addr, tx))
}

/// Run the shared MCP Streamable HTTP background daemon with hourly cache pruning until
/// interrupted (`SIGINT` / `Ctrl-C`).
///
/// # Errors
/// Returns an error if the bind address is invalid or the listener cannot start.
pub async fn run_streamable_http_server(bind_addr: &str) -> Result<(), String> {
    let (addr, shutdown) = spawn_streamable_http_server(bind_addr).await?;
    tracing::info!(
        "fast-docs daemon listening on http://{addr}/mcp (health: http://{addr}/health)"
    );

    let prune_task = tokio::spawn(async {
        loop {
            tokio::time::sleep(DAEMON_PRUNE_INTERVAL).await;
            if let Some(cache) = DiskCache::default_user_cache() {
                let removed = cache.prune_expired(SystemTime::now());
                if removed > 0 {
                    tracing::info!(removed, "pruned expired cache entries");
                }
            }
        }
    });

    let _ = tokio::signal::ctrl_c().await;
    prune_task.abort();
    let _ = shutdown.send(());
    Ok(())
}
