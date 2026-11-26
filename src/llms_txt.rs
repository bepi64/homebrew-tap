use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use reqwest::redirect::Policy;
use reqwest::Url;

const MAX_TARGET_LEN: usize = 300;
const MAX_REDIRECTS: usize = 5;

/// Forbidden internal/private host suffixes that must never be fetched via `llms.txt`.
const FORBIDDEN_HOST_SUFFIXES: [&str; 6] = [
    ".localhost",
    ".local",
    ".internal",
    ".consul",
    ".home.arpa",
    ".lan",
];

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let is_cgnat = a == 100 && (64..=127).contains(&b);
    let is_benchmark = a == 198 && (18..=19).contains(&b);
    let is_ietf_protocol = a == 192 && b == 0 && c == 0;
    let is_reserved = a >= 240;

    !ip.is_unspecified()
        && !ip.is_loopback()
        && !ip.is_private()
        && !ip.is_link_local()
        && !ip.is_broadcast()
        && !ip.is_documentation()
        && !ip.is_multicast()
        && !is_cgnat
        && !is_benchmark
        && !is_ietf_protocol
        && !is_reserved
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(mapped_v4) = ip.to_ipv4_mapped() {
        return is_public_ipv4(mapped_v4);
    }

    let segments = ip.segments();
    let is_ula = (segments[0] & 0xfe00) == 0xfc00;
    let is_link_local = (segments[0] & 0xffc0) == 0xfe80;
    let is_documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
    // RFC 6052 well-known NAT64 prefix (64:ff9b::/96) embeds an IPv4 address in the last 32 bits.
    let is_nat64 = segments[0] == 0x0064
        && segments[1] == 0xff9b
        && segments[2] == 0
        && segments[3] == 0
        && segments[4] == 0
        && segments[5] == 0;

    if is_nat64 {
        let embedded = Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            (segments[6] & 0xff) as u8,
            (segments[7] >> 8) as u8,
            (segments[7] & 0xff) as u8,
        );
        return is_public_ipv4(embedded);
    }

    !ip.is_unspecified()
        && !ip.is_loopback()
        && !ip.is_multicast()
        && !is_ula
        && !is_link_local
        && !is_documentation
}

/// Returns `true` only when `ip` is globally routable (blocking loopback, RFC 1918 private,
/// CGNAT, link-local / cloud IMDS, ULA, NAT64-private, multicast, and documentation ranges).
#[must_use]
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_ipv4(v4),
        IpAddr::V6(v6) => is_public_ipv6(v6),
    }
}

fn validate_public_hostname(host: &str) -> Result<(), String> {
    let lower = host.to_ascii_lowercase();
    if lower.is_empty() || lower == "localhost" || !lower.contains('.') {
        return Err(format!("forbidden internal or unqualified host '{host}'"));
    }
    if lower.starts_with('.') || lower.ends_with('.') || lower.contains("..") {
        return Err(format!("malformed hostname '{host}'"));
    }
    if lower.parse::<IpAddr>().is_ok() || (lower.starts_with('[') && lower.ends_with(']')) {
        return Err("IP literal hosts are forbidden for llms.txt requests".to_string());
    }
    for suffix in FORBIDDEN_HOST_SUFFIXES {
        if lower.ends_with(suffix) {
            return Err(format!("forbidden internal host suffix in '{host}'"));
        }
    }
    Ok(())
}

/// Normalize and strictly validate a `llms.txt` or `llms-full.txt` documentation URL.
///
/// Accepts bare domains (`hono.dev`), domain + prefix (`example.com/docs`), or explicit HTTPS
/// URLs (`https://vite.dev/llms.txt`). Rejects non-HTTPS schemes, explicit ports, credentials,
/// queries, fragments, IP literals, internal hostnames, and path traversal.
///
/// # Errors
/// Returns an error if `target` is malformed or violates the SSRF safety policy.
pub fn build_llms_txt_url(target: &str, full: bool) -> Result<Url, String> {
    let trimmed = target.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_TARGET_LEN {
        return Err("invalid llms.txt target: empty or exceeds maximum length".to_string());
    }
    if trimmed.contains('\\')
        || trimmed.contains("..")
        || trimmed.chars().any(char::is_control)
        || trimmed.contains(char::is_whitespace)
    {
        return Err("invalid llms.txt target: forbidden characters or path traversal".to_string());
    }

    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };

    let mut url = Url::parse(&candidate).map_err(|err| format!("invalid URL: {err}"))?;

    if url.scheme() != "https" {
        return Err(format!("forbidden URL scheme '{}'", url.scheme()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs with embedded credentials are forbidden".to_string());
    }
    if url.port().is_some() {
        return Err("URLs with explicit ports are forbidden".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("query strings and fragments are forbidden for llms.txt URLs".to_string());
    }

    let host = url
        .host_str()
        .ok_or_else(|| "URL is missing a host".to_string())?
        .to_string();
    validate_public_hostname(&host)?;

    let desired_file = if full { "llms-full.txt" } else { "llms.txt" };
    let clean_path = url.path().trim_end_matches('/');

    let final_path = if let Some(prefix) = clean_path
        .strip_suffix("/llms.txt")
        .or_else(|| clean_path.strip_suffix("/llms-full.txt"))
    {
        format!("{prefix}/{desired_file}")
    } else if clean_path.is_empty() {
        format!("/{desired_file}")
    } else {
        format!("{clean_path}/{desired_file}")
    };

    url.set_path(&final_path);
    Ok(url)
}

/// Resolve `host:443` via DNS and verify that **every** returned IP address is globally routable.
///
/// # Errors
/// Returns an error if DNS resolution fails, yields no addresses, or resolves to any private,
/// loopback, link-local, or otherwise non-public IP address.
pub async fn resolve_public_socket_addrs(host: &str) -> Result<Vec<SocketAddr>, String> {
    validate_public_hostname(host)?;
    let resolved = tokio::net::lookup_host((host, 443))
        .await
        .map_err(|err| format!("DNS lookup failed for '{host}': {err}"))?;

    let mut addrs = Vec::new();
    for addr in resolved {
        if !is_public_ip(addr.ip()) {
            return Err(format!(
                "blocked SSRF target: '{host}' resolved to non-public IP {}",
                addr.ip()
            ));
        }
        addrs.push(addr);
    }

    if addrs.is_empty() {
        return Err(format!("DNS lookup returned no addresses for '{host}'"));
    }

    Ok(addrs)
}

/// Build an HTTP client pinned to pre-validated public IP addresses for `host` (preventing DNS
/// rebinding) and restricting redirects to HTTPS on the same host.
///
/// # Errors
/// Returns an error if the underlying TLS/HTTP client cannot be constructed.
pub fn build_pinned_llms_client(
    host: &str,
    addrs: &[SocketAddr],
    user_agent: &str,
    connect_timeout: Duration,
) -> Result<reqwest::Client, String> {
    let expected_host = host.to_ascii_lowercase();
    let redirect_policy = Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        let target = attempt.url();
        if target.scheme() != "https"
            || target.port().is_some()
            || !target.username().is_empty()
            || target.password().is_some()
        {
            return attempt.error("blocked redirect: unsafe scheme, port, or credentials");
        }
        let Some(redirect_host) = target.host_str() else {
            return attempt.error("blocked redirect: missing host");
        };
        if !redirect_host.eq_ignore_ascii_case(&expected_host) {
            return attempt.error("blocked redirect: cross-host redirect forbidden for llms.txt");
        }
        attempt.follow()
    });

    reqwest::Client::builder()
        .user_agent(user_agent)
        .connect_timeout(connect_timeout)
        .redirect(redirect_policy)
        .resolve_to_addrs(host, addrs)
        .build()
        .map_err(|err| format!("failed to build HTTP client: {err}"))
}
