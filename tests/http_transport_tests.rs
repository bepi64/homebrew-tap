use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use fast_docs::http_server::{
    parse_cli_mode, parse_loopback_bind_addr, spawn_streamable_http_server, RunMode,
};

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

#[test]
fn parse_cli_mode_defaults_to_http_daemon_and_supports_stdio_override() {
    let bin = vec!["fast-docs".to_string()];
    assert_eq!(
        parse_cli_mode(&bin, None),
        RunMode::Http("127.0.0.1:3000".to_string())
    );
    assert_eq!(
        parse_cli_mode(&bin, Some("127.0.0.1:4123")),
        RunMode::Http("127.0.0.1:4123".to_string())
    );

    let explicit_http = vec![
        "fast-docs".to_string(),
        "--http".to_string(),
        "127.0.0.1:8080".to_string(),
    ];
    assert_eq!(
        parse_cli_mode(&explicit_http, Some("127.0.0.1:4123")),
        RunMode::Http("127.0.0.1:8080".to_string())
    );

    let eq_http = vec!["fast-docs".to_string(), "--http=127.0.0.1:9090".to_string()];
    assert_eq!(
        parse_cli_mode(&eq_http, None),
        RunMode::Http("127.0.0.1:9090".to_string())
    );

    let stdio_args = vec!["fast-docs".to_string(), "--stdio".to_string()];
    assert_eq!(parse_cli_mode(&stdio_args, None), RunMode::Stdio);
}

#[tokio::test]
async fn streamable_http_server_handles_initialize_health_404_and_dns_rebinding() {
    let started = spawn_streamable_http_server("127.0.0.1:0").await;
    assert!(started.is_ok());
    let Ok((addr, shutdown)) = started else {
        return;
    };

    let client = reqwest::Client::new();

    // 1. Health endpoint returns 200 OK JSON
    let health = client.get(format!("http://{addr}/health")).send().await;
    assert!(health.is_ok());
    if let Ok(resp) = health {
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let text = resp.text().await.unwrap_or_default();
        assert!(
            text.contains("\"status\":\"ok\""),
            "unexpected health: {text}"
        );
        assert!(text.contains("fast-docs"));
    }

    // 2. Unknown route returns 404 Not Found
    let unknown = client.get(format!("http://{addr}/unknown")).send().await;
    assert!(unknown.is_ok());
    if let Ok(resp) = unknown {
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    // 3. Valid loopback MCP initialize succeeds and advertises fast-docs
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

    // 4. DNS rebinding probe with hostile Host header must be rejected (HTTP 403)
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

#[tokio::test]
async fn streamable_http_server_serves_multiple_concurrent_clients() {
    let started = spawn_streamable_http_server("127.0.0.1:0").await;
    assert!(started.is_ok());
    let Ok((addr, shutdown)) = started else {
        return;
    };

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");

    let make_req = |id: u32| {
        let client = client.clone();
        let url = url.clone();
        async move {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": format!("agent-{id}"), "version": "1.0.0" }
                }
            });
            client
                .post(&url)
                .header("Accept", "application/json, text/event-stream")
                .json(&body)
                .send()
                .await
        }
    };

    let (r1, r2, r3, r4) = tokio::join!(make_req(1), make_req(2), make_req(3), make_req(4));
    for res in [r1, r2, r3, r4] {
        assert!(res.is_ok());
        if let Ok(resp) = res {
            assert!(resp.status().is_success());
        }
    }

    let _ = shutdown.send(());
}
