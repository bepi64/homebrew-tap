use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::tools::FastDocsServer;

/// Default loopback bind address for `--http` mode.
pub const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:3000";

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

/// Spawn the MCP Streamable HTTP server on a loopback address (endpoint `/mcp`) with `rmcp`'s
/// built-in DNS rebinding protection enabled. Returns the bound `SocketAddr` and a shutdown trigger.
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

    let service = Arc::new(StreamableHttpService::new(
        || Ok(FastDocsServer::new()),
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
                                Ok::<_, Infallible>(svc.handle(req).await)
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

/// Run the MCP Streamable HTTP server until interrupted (Ctrl-C).
///
/// # Errors
/// Returns an error if the bind address is invalid or the listener cannot start.
pub async fn run_streamable_http_server(bind_addr: &str) -> Result<(), String> {
    let (addr, shutdown) = spawn_streamable_http_server(bind_addr).await?;
    tracing::info!("fast-docs Streamable HTTP MCP server listening on http://{addr}/mcp");
    let _ = tokio::signal::ctrl_c().await;
    let _ = shutdown.send(());
    Ok(())
}
