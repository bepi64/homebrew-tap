use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use fast_docs::http_server::{parse_loopback_bind_addr, spawn_streamable_http_server};

#[test]
fn parse_loopback_bind_addr_allows_loopback_and_rejects_non_loopback() {
    let ok_v4 = parse_loopback_bind_addr("127.0.0.1:3000");
    assert!(ok_v4.is_ok());
    if let Ok(addr) = ok_v4 {
        assert_eq!(addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(addr.port(), 3000);
    }

    let ok_v6 = parse_loopback_bind_addr("[::1]:3001");
    assert!(ok_v6.is_ok());
    if let Ok(addr) = ok_v6 {
        assert_eq!(addr.ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
        assert_eq!(addr.port(), 3001);
    }

    for rejected in [
        "0.0.0.0:3000",
        "[::]:3000",
        "192.168.1.10:3000",
        "not-an-addr",
    ] {
        assert!(
            parse_loopback_bind_addr(rejected).is_err(),
            "must reject non-loopback or invalid bind address {rejected:?}"
        );
    }
}

#[tokio::test]
async fn streamable_http_server_handles_initialize_and_blocks_dns_rebinding() {
    let started = spawn_streamable_http_server("127.0.0.1:0").await;
    assert!(started.is_ok());
    let Ok((addr, shutdown)) = started else {
        return;
    };

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let init_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "http-test", "version": "1.0.0" }
        }
    });

    // 1. Valid loopback request succeeds and advertises fast-docs
    let resp = client
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .json(&init_body)
        .send()
        .await;
    assert!(resp.is_ok());
    if let Ok(resp) = resp {
        assert!(
            resp.status().is_success(),
            "expected 2xx, got {}",
            resp.status()
        );
        let text = resp.text().await.unwrap_or_default();
        assert!(text.contains("fast-docs"), "unexpected body: {text}");
    }

    // 2. DNS rebinding probe with hostile Host header must be rejected (HTTP 403)
    let rebind = client
        .post(&url)
        .header("Host", "evil.example.com")
        .header("Accept", "application/json, text/event-stream")
        .json(&init_body)
        .send()
        .await;
    assert!(rebind.is_ok());
    if let Ok(resp) = rebind {
        assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
    }

    let _ = shutdown.send(());
}
