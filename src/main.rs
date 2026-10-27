use rmcp::{
    ServiceExt,
    transport::stdio,
};
use std::io;
use tracing_subscriber::{fmt, EnvFilter};

mod tools;

extern "C" {
    fn cache_prefetch_init() -> i32;
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .with_writer(io::stderr)
        .try_init();

    unsafe { cache_prefetch_init(); }

    let service = tools::FastDocsServer::new()
        .serve(stdio())
        .await
        .inspect_err(|e| eprintln!("Error starting server: {e}"))?;

    service.waiting().await?;
    Ok(())
}
