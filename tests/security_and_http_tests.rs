use std::time::Duration;

use fast_docs::tools::{
    build_url, build_url_with_module, fetch_url_conditional, fetch_url_with_limits,
    validate_outbound_url, Ecosystem, FetchOutcome, ALLOWED_HOSTS, CONNECT_TIMEOUT,
    MAX_RESPONSE_BYTES, REQUEST_TIMEOUT,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[test]
fn network_hardening_constants_match_documented_bounds() {
    assert_eq!(MAX_RESPONSE_BYTES, 5 * 1024 * 1024);
    assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(10));
    assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(5));
    assert_eq!(
        ALLOWED_HOSTS,
        [
            "docs.rs",
            "pypi.org",
            "pkg.go.dev",
            "api.github.com",
            "registry.npmjs.org",
            "unpkg.com",
        ]
    );
}

#[test]
fn build_url_accepts_valid_libraries_and_versions() {
    assert_eq!(
        build_url("tokio", "latest", Ecosystem::Rust).as_deref(),
        Ok("https://docs.rs/tokio/latest")
    );
    assert_eq!(
        build_url("serde_json", "1.0.140", Ecosystem::Rust).as_deref(),
        Ok("https://docs.rs/serde_json/1.0.140")
    );
    assert_eq!(
        build_url("flask", "latest", Ecosystem::Python).as_deref(),
        Ok("https://pypi.org/pypi/flask/json")
    );
    assert_eq!(
        build_url("flask", "3.1.0", Ecosystem::Python).as_deref(),
        Ok("https://pypi.org/pypi/flask/3.1.0/json")
    );
    assert_eq!(
        build_url("gin", "latest", Ecosystem::Go).as_deref(),
        Ok("https://pkg.go.dev/github.com/gin-gonic/gin")
    );
    assert_eq!(
        build_url("github.com/gin-gonic/gin", "v1.10.0", Ecosystem::Go).as_deref(),
        Ok("https://pkg.go.dev/github.com/gin-gonic/gin@v1.10.0")
    );
    assert_eq!(
        build_url("zod", "latest", Ecosystem::Npm).as_deref(),
        Ok("https://unpkg.com/zod@latest/README.md")
    );
    assert_eq!(
        build_url("react", "19.0.0", Ecosystem::Npm).as_deref(),
        Ok("https://unpkg.com/react@19.0.0/README.md")
    );
    assert_eq!(
        build_url("@modelcontextprotocol/sdk", "latest", Ecosystem::Npm).as_deref(),
        Ok("https://unpkg.com/@modelcontextprotocol/sdk@latest/README.md")
    );
    assert_eq!(
        build_url("@types/node", "22.0.0", Ecosystem::Npm).as_deref(),
        Ok("https://unpkg.com/@types/node@22.0.0/README.md")
    );
}

#[test]
fn build_url_rejects_malformed_npm_scoped_packages() {
    let bad_npm = [
        "@scope",
        "@/pkg",
        "@scope/",
        "@scope/a/b",
        "@scope/../pkg",
        "@../pkg",
        "pkg@evil.com",
        "@@scope/pkg",
        "@scope/pkg@1.0.0",
        "react/extra",
    ];
    for bad in bad_npm {
        assert!(
            build_url(bad, "latest", Ecosystem::Npm).is_err(),
            "Npm build_url should reject {bad:?}"
        );
    }
}

#[test]
fn build_url_with_module_resolves_subpages_for_rust_and_go() {
    assert_eq!(
        build_url_with_module("tokio", "latest", Ecosystem::Rust, Some("sync")).as_deref(),
        Ok("https://docs.rs/tokio/latest/tokio/sync/")
    );
    assert_eq!(
        build_url_with_module("serde-json", "1.0.140", Ecosystem::Rust, Some("de")).as_deref(),
        Ok("https://docs.rs/serde-json/1.0.140/serde_json/de/")
    );
    assert_eq!(
        build_url_with_module("tokio", "latest", Ecosystem::Rust, Some("sync::mpsc")).as_deref(),
        Ok("https://docs.rs/tokio/latest/tokio/sync/mpsc/")
    );
    assert_eq!(
        build_url_with_module(
            "tokio",
            "latest",
            Ecosystem::Rust,
            Some("sync/struct.RwLock.html")
        )
        .as_deref(),
        Ok("https://docs.rs/tokio/latest/tokio/sync/struct.RwLock.html")
    );
    assert_eq!(
        build_url_with_module("gin", "latest", Ecosystem::Go, Some("binding")).as_deref(),
        Ok("https://pkg.go.dev/github.com/gin-gonic/gin/binding")
    );
    assert_eq!(
        build_url_with_module("gin", "v1.10.0", Ecosystem::Go, Some("binding")).as_deref(),
        Ok("https://pkg.go.dev/github.com/gin-gonic/gin@v1.10.0/binding")
    );
    // Python PyPI JSON API is package-scoped; module does not alter the JSON endpoint URL
    assert_eq!(
        build_url_with_module("flask", "latest", Ecosystem::Python, Some("blueprints")).as_deref(),
        Ok("https://pypi.org/pypi/flask/json")
    );
}

#[test]
fn build_url_with_module_rejects_ssrf_and_traversal_in_module() {
    let malicious_modules = [
        "../secret",
        "sync/../../etc",
        "@evil.com",
        "sync?x=1",
        "sync#frag",
        "/leading",
        "trailing/",
        "double//slash",
        "back\\slash",
        "has space",
        "https://evil.com/x",
        "null\0byte",
    ];

    for bad in malicious_modules {
        assert!(
            build_url_with_module("tokio", "latest", Ecosystem::Rust, Some(bad)).is_err(),
            "Rust module path should reject {bad:?}"
        );
        assert!(
            build_url_with_module("gin", "latest", Ecosystem::Go, Some(bad)).is_err(),
            "Go module path should reject {bad:?}"
        );
    }

    let overlong = "a".repeat(201);
    assert!(build_url_with_module("tokio", "latest", Ecosystem::Rust, Some(&overlong)).is_err());
}

#[test]
fn build_url_rejects_ssrf_path_traversal_and_malformed_inputs() {
    let malicious_libraries = [
        "",
        "   ",
        "../admin",
        "tokio/../../secret",
        "tokio@evil.com",
        "https://evil.com/pwn",
        "http://169.254.169.254/latest/meta-data",
        "tokio?foo=bar",
        "tokio#frag",
        "tokio\\evil",
        "/leading-slash",
        "trailing-slash/",
        "double//slash",
        "has space",
        "has\nnewline",
        "has\0null",
    ];

    for bad in malicious_libraries {
        assert!(
            build_url(bad, "latest", Ecosystem::Rust).is_err(),
            "Rust build_url should reject {bad:?}"
        );
        assert!(
            build_url(bad, "latest", Ecosystem::Python).is_err(),
            "Python build_url should reject {bad:?}"
        );
        assert!(
            build_url(bad, "latest", Ecosystem::Go).is_err(),
            "Go build_url should reject {bad:?}"
        );
    }

    let malicious_versions = [
        "../1.0.0",
        "1.0.0@evil.com",
        "1.0.0?x=1",
        "1.0.0#frag",
        "1.0.0/extra",
        "1.0.0\\x",
        "1.0.0 bad",
    ];

    for bad_ver in malicious_versions {
        assert!(
            build_url("tokio", bad_ver, Ecosystem::Rust).is_err(),
            "build_url should reject version {bad_ver:?}"
        );
    }

    let overlong = "a".repeat(201);
    assert!(build_url(&overlong, "latest", Ecosystem::Rust).is_err());
}

#[test]
fn validate_outbound_url_enforces_https_and_host_allowlist() {
    assert!(validate_outbound_url("https://docs.rs/tokio/latest").is_ok());
    assert!(validate_outbound_url("https://pypi.org/pypi/flask/json").is_ok());
    assert!(validate_outbound_url("https://pkg.go.dev/github.com/gin-gonic/gin").is_ok());
    assert!(validate_outbound_url("https://api.github.com/search/repositories?q=rust").is_ok());
    assert!(validate_outbound_url("https://registry.npmjs.org/-/v1/search?text=react").is_ok());
    assert!(validate_outbound_url("https://unpkg.com/@types/node@22.0.0/README.md").is_ok());

    let rejected = [
        "http://docs.rs/tokio/latest",
        "https://evil.com/tokio",
        "https://docs.rs.evil.com/tokio",
        "https://169.254.169.254/latest/meta-data/",
        "https://127.0.0.1:8080/internal",
        "https://user:pass@docs.rs/tokio",
        "https://docs.rs:8443/tokio",
        "file:///etc/passwd",
        "not a url",
    ];

    for url in rejected {
        assert!(
            validate_outbound_url(url).is_err(),
            "validate_outbound_url must reject {url:?}"
        );
    }
}

async fn spawn_mock_http_server(response: Vec<u8>, delay: Duration) -> Option<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await.ok()?;
    let addr = listener.local_addr().ok()?;

    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0_u8; 1024];
            let _ = socket.read(&mut buf).await;
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let _ = socket.write_all(&response).await;
            let _ = socket.shutdown().await;
        }
    });

    Some(format!("http://{addr}/test"))
}

#[tokio::test]
async fn fetch_enforces_streaming_response_size_limit() {
    let body = "x".repeat(2048);
    let raw = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let url = spawn_mock_http_server(raw.into_bytes(), Duration::ZERO).await;
    assert!(url.is_some());
    let client = reqwest::Client::new();

    if let Some(url) = url {
        // Limit below Content-Length must fail immediately
        let err = fetch_url_with_limits(&client, &url, Duration::from_secs(2), 1024).await;
        assert!(err.is_err());
    }

    // Response without Content-Length (streaming check) must also fail when exceeding cap
    let chunked_raw = format!("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{body}");
    let url_stream = spawn_mock_http_server(chunked_raw.into_bytes(), Duration::ZERO).await;
    assert!(url_stream.is_some());
    if let Some(url_stream) = url_stream {
        let err_stream =
            fetch_url_with_limits(&client, &url_stream, Duration::from_secs(2), 1024).await;
        assert!(err_stream.is_err());
    }

    // Limit at or above body length must succeed
    let raw_ok = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let url_ok = spawn_mock_http_server(raw_ok.into_bytes(), Duration::ZERO).await;
    assert!(url_ok.is_some());
    if let Some(url_ok) = url_ok {
        let ok = fetch_url_with_limits(&client, &url_ok, Duration::from_secs(2), 2048).await;
        assert_eq!(ok.map(|s| s.len()), Ok(2048));
    }
}

#[tokio::test]
async fn fetch_enforces_request_timeout() {
    let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec();
    let url = spawn_mock_http_server(raw, Duration::from_millis(400)).await;
    assert!(url.is_some());
    let client = reqwest::Client::new();

    if let Some(url) = url {
        let result = fetch_url_with_limits(&client, &url, Duration::from_millis(80), 4096).await;
        assert!(result.is_err());
        if let Err(msg) = result {
            assert!(msg.contains("timeout"), "unexpected error: {msg}");
        }
    }
}

#[tokio::test]
async fn fetch_reports_non_success_http_status() {
    let raw = b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found"
        .to_vec();
    let url = spawn_mock_http_server(raw, Duration::ZERO).await;
    assert!(url.is_some());
    let client = reqwest::Client::new();

    if let Some(url) = url {
        let result = fetch_url_with_limits(&client, &url, Duration::from_secs(2), 4096).await;
        assert!(result.is_err());
        if let Err(msg) = result {
            assert!(msg.contains("404"), "unexpected error: {msg}");
        }
    }
}

#[tokio::test]
async fn fetch_rejects_invalid_utf8_body() {
    let mut raw = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\n".to_vec();
    raw.extend_from_slice(&[0xFF, 0xFE, 0xFD]);

    let url = spawn_mock_http_server(raw, Duration::ZERO).await;
    assert!(url.is_some());
    let client = reqwest::Client::new();

    if let Some(url) = url {
        let result = fetch_url_with_limits(&client, &url, Duration::from_secs(2), 4096).await;
        assert!(result.is_err());
        if let Err(msg) = result {
            assert!(msg.contains("UTF-8"), "unexpected error: {msg}");
        }
    }
}

#[tokio::test]
async fn fetch_conditional_sends_if_none_match_and_handles_304() {
    let Ok(listener) = TcpListener::bind("127.0.0.1:0").await else {
        return;
    };
    let Ok(addr) = listener.local_addr() else {
        return;
    };

    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0_u8; 2048];
            let n = socket.read(&mut buf).await.unwrap_or(0);
            let req_text = String::from_utf8_lossy(&buf[..n]);
            let response = if req_text
                .to_ascii_lowercase()
                .contains("if-none-match: \"etag-42\"")
            {
                b"HTTP/1.1 304 Not Modified\r\nETag: \"etag-42\"\r\nConnection: close\r\n\r\n"
                    .to_vec()
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nmiss".to_vec()
            };
            let _ = socket.write_all(&response).await;
            let _ = socket.shutdown().await;
        }
    });

    let url = format!("http://{addr}/docs");
    let client = reqwest::Client::new();
    let outcome = fetch_url_conditional(
        &client,
        &url,
        Some("\"etag-42\""),
        Duration::from_secs(2),
        4096,
    )
    .await;

    assert_eq!(
        outcome,
        Ok(FetchOutcome::NotModified {
            etag: Some("\"etag-42\"".to_string()),
        })
    );
}

#[tokio::test]
async fn fetch_conditional_extracts_etag_on_200_ok() {
    let raw =
        b"HTTP/1.1 200 OK\r\nETag: \"abc123\"\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
            .to_vec();
    let url = spawn_mock_http_server(raw, Duration::ZERO).await;
    assert!(url.is_some());
    let client = reqwest::Client::new();

    if let Some(url) = url {
        let outcome =
            fetch_url_conditional(&client, &url, None, Duration::from_secs(2), 4096).await;
        assert_eq!(
            outcome,
            Ok(FetchOutcome::Fetched {
                body: "hello".to_string(),
                etag: Some("\"abc123\"".to_string()),
            })
        );
    }
}
