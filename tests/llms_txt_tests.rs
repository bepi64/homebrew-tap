use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use fast_docs::llms_txt::{build_llms_txt_url, is_public_ip};
use proptest::prelude::*;

#[test]
fn build_llms_txt_url_normalizes_domains_and_urls() {
    assert_eq!(
        build_llms_txt_url("hono.dev", false)
            .map(|u| u.to_string())
            .as_deref(),
        Ok("https://hono.dev/llms.txt")
    );
    assert_eq!(
        build_llms_txt_url("hono.dev", true)
            .map(|u| u.to_string())
            .as_deref(),
        Ok("https://hono.dev/llms-full.txt")
    );
    assert_eq!(
        build_llms_txt_url("https://docs.pydantic.dev/", false)
            .map(|u| u.to_string())
            .as_deref(),
        Ok("https://docs.pydantic.dev/llms.txt")
    );
    assert_eq!(
        build_llms_txt_url("https://vite.dev/llms.txt", false)
            .map(|u| u.to_string())
            .as_deref(),
        Ok("https://vite.dev/llms.txt")
    );
    assert_eq!(
        build_llms_txt_url("https://vite.dev/llms.txt", true)
            .map(|u| u.to_string())
            .as_deref(),
        Ok("https://vite.dev/llms-full.txt")
    );
    assert_eq!(
        build_llms_txt_url("https://example.com/docs", false)
            .map(|u| u.to_string())
            .as_deref(),
        Ok("https://example.com/docs/llms.txt")
    );
}

#[test]
fn build_llms_txt_url_rejects_ssrf_and_internal_targets() {
    let rejected = [
        "",
        "   ",
        "http://hono.dev/llms.txt",
        "file:///etc/passwd",
        "localhost",
        "https://localhost/llms.txt",
        "https://app.localhost/llms.txt",
        "https://printer.local/llms.txt",
        "https://db.internal/llms.txt",
        "https://prod.service.consul/llms.txt",
        "intranet",
        "127.0.0.1",
        "https://127.0.0.1/llms.txt",
        "https://169.254.169.254/latest/meta-data/",
        "https://10.0.0.1/llms.txt",
        "https://[::1]/llms.txt",
        "https://user:pass@hono.dev/llms.txt",
        "https://hono.dev:8443/llms.txt",
        "https://hono.dev/llms.txt?foo=bar",
        "https://hono.dev/llms.txt#frag",
        "https://hono.dev/../secret/llms.txt",
        "https://hono.dev\\evil.com/llms.txt",
    ];

    for bad in rejected {
        assert!(
            build_llms_txt_url(bad, false).is_err(),
            "build_llms_txt_url must reject {bad:?}"
        );
    }
}

#[test]
fn is_public_ip_blocks_all_private_local_and_metadata_ranges() {
    let private_or_special = [
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)), // CGNAT
        IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)), // Cloud IMDS
        IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)),
        IpAddr::V4(Ipv4Addr::new(172, 31, 255, 254)),
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), // Documentation
        IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)), // Multicast
        IpAddr::V4(Ipv4Addr::BROADCAST),
        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1)), // ULA
        IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)), // Link-local
        IpAddr::V6(Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1)), // Multicast
        IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 1)), // Documentation
        IpAddr::V6(Ipv6Addr::new(0x0064, 0xff9b, 0, 0, 0, 0, 0x0a00, 0x0001)), // NAT64 -> 10.0.0.1
        IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped()),
        IpAddr::V6(Ipv4Addr::new(169, 254, 169, 254).to_ipv6_mapped()),
    ];

    for ip in private_or_special {
        assert!(!is_public_ip(ip), "is_public_ip must reject {ip}");
    }

    let public = [
        IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
        IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
        IpAddr::V4(Ipv4Addr::new(172, 32, 0, 1)),
        IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111)),
    ];

    for ip in public {
        assert!(is_public_ip(ip), "is_public_ip should allow {ip}");
    }
}

proptest! {
    #[test]
    fn build_llms_txt_url_never_panics_and_enforces_invariants(
        raw in ".{0,150}",
        full in any::<bool>()
    ) {
        if let Ok(url) = build_llms_txt_url(&raw, full) {
            prop_assert_eq!(url.scheme(), "https");
            prop_assert!(url.port().is_none());
            prop_assert!(url.username().is_empty());
            prop_assert!(url.password().is_none());
            prop_assert!(url.query().is_none());
            prop_assert!(url.fragment().is_none());
            let path = url.path();
            prop_assert!(path.ends_with("/llms.txt") || path.ends_with("/llms-full.txt"));
        }
    }
}
