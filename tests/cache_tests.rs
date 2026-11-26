use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fast_docs::cache::{
    cache_key, DiskCache, DEFAULT_CACHE_TTL, PINNED_CACHE_TTL, SEARCH_CACHE_TTL,
};

fn unique_temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!(
        "fast-docs-test-{label}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn cache_key_is_deterministic_fixed_hex_and_path_safe() {
    let url = "https://docs.rs/../../etc/passwd?a=1#frag";
    let key1 = cache_key(url);
    let key2 = cache_key(url);

    assert_eq!(key1, key2);
    assert_eq!(key1.len(), 32);
    assert!(key1.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(!key1.contains('/'));
    assert!(!key1.contains(".."));
}

#[test]
fn cache_key_distinguishes_different_urls() {
    let a = cache_key("https://docs.rs/tokio/latest");
    let b = cache_key("https://docs.rs/serde/latest");
    assert_ne!(a, b);
}

#[test]
fn ttl_constants_match_documented_policy() {
    assert_eq!(DEFAULT_CACHE_TTL, Duration::from_hours(24));
    assert_eq!(PINNED_CACHE_TTL, Duration::from_hours(168));
}

#[test]
fn disk_cache_roundtrip_and_ttl_expiration() {
    let dir = unique_temp_dir("roundtrip");
    let cache = DiskCache::new(dir.clone(), DEFAULT_CACHE_TTL, PINNED_CACHE_TTL);

    let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let url = "https://docs.rs/tokio/latest";

    assert!(cache.put_at(url, "tokio docs body", false, t0).is_ok());

    // Fresh right on the 24h boundary (>)
    let t_boundary = t0 + Duration::from_hours(24);
    assert_eq!(
        cache.get_at(url, false, t_boundary).as_deref(),
        Some("tokio docs body")
    );

    // Expired after 24h + 1s for mutable ("latest") entry
    let t_expired = t0 + Duration::from_hours(24) + Duration::from_secs(1);
    assert_eq!(cache.get_at(url, false, t_expired), None);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn disk_cache_wall_clock_put_get_and_default_user_cache() {
    assert!(DiskCache::default_user_cache().is_some());

    let dir = unique_temp_dir("wallclock");
    let cache = DiskCache::new(dir.clone(), DEFAULT_CACHE_TTL, PINNED_CACHE_TTL);
    let url = "https://docs.rs/serde/latest";

    assert_eq!(cache.get(url, false), None);
    assert!(cache.put(url, "serde docs", false).is_ok());
    assert_eq!(cache.get(url, false).as_deref(), Some("serde docs"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn disk_cache_pinned_version_uses_longer_ttl() {
    let dir = unique_temp_dir("pinned");
    let cache = DiskCache::new(dir.clone(), DEFAULT_CACHE_TTL, PINNED_CACHE_TTL);

    let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let url = "https://docs.rs/tokio/1.40.0";

    assert!(cache.put_at(url, "tokio 1.40.0 docs", true, t0).is_ok());

    // Still fresh after 3 days for a version-pinned entry, even if caller passes pinned=false
    let t_three_days = t0 + Duration::from_hours(72);
    assert_eq!(
        cache.get_at(url, false, t_three_days).as_deref(),
        Some("tokio 1.40.0 docs")
    );

    // Expired after 7 days + 1 second
    let t_eight_days = t0 + Duration::from_hours(168) + Duration::from_secs(1);
    assert_eq!(cache.get_at(url, true, t_eight_days), None);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn disk_cache_ignores_corrupted_entries() {
    let dir = unique_temp_dir("corrupt");
    assert!(fs::create_dir_all(&dir).is_ok());

    let url = "https://pypi.org/pypi/flask/json";
    let path = dir.join(cache_key(url));
    assert!(fs::write(&path, "not-valid-json{{{").is_ok());

    let cache = DiskCache::new(dir.clone(), DEFAULT_CACHE_TTL, PINNED_CACHE_TTL);
    let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert_eq!(cache.get_at(url, false, now), None);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn disk_cache_prune_expired_removes_only_stale_files() {
    let dir = unique_temp_dir("prune");
    let cache = DiskCache::new(dir.clone(), DEFAULT_CACHE_TTL, PINNED_CACHE_TTL);

    let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let old_url = "https://docs.rs/old/latest";
    let pinned_url = "https://docs.rs/pinned/1.0.0";
    let fresh_url = "https://docs.rs/fresh/latest";

    assert!(cache.put_at(old_url, "old", false, t0).is_ok());
    assert!(cache.put_at(pinned_url, "pinned", true, t0).is_ok());
    assert!(cache
        .put_at(fresh_url, "fresh", false, t0 + Duration::from_hours(20))
        .is_ok());

    let exact_boundary_url = "https://docs.rs/boundary/latest";
    assert!(cache
        .put_at(
            exact_boundary_url,
            "boundary",
            false,
            t0 + Duration::from_hours(1)
        )
        .is_ok());

    // Place a corrupt entry and an outdated schema version entry that prune should clean up
    assert!(fs::write(dir.join("deadbeef00000000"), "corrupt").is_ok());
    assert!(fs::write(
        dir.join("00000000deadbeef"),
        r#"{"version":1,"fetched_at_secs":1700000000,"pinned":true,"content":"legacy"}"#
    )
    .is_ok());

    let prune_time = t0 + Duration::from_hours(25);
    let removed = cache.prune_expired(prune_time);
    assert_eq!(removed, 3);

    assert_eq!(cache.get_at(old_url, false, prune_time), None);
    assert_eq!(
        cache
            .get_at(exact_boundary_url, false, prune_time)
            .as_deref(),
        Some("boundary")
    );
    assert_eq!(
        cache.get_at(pinned_url, true, prune_time).as_deref(),
        Some("pinned")
    );
    assert_eq!(
        cache.get_at(fresh_url, false, prune_time).as_deref(),
        Some("fresh")
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn search_cache_ttl_is_fifteen_minutes() {
    assert_eq!(SEARCH_CACHE_TTL, Duration::from_mins(15));
}

#[test]
fn disk_cache_etag_supports_stale_lookup_and_refresh() {
    let dir = unique_temp_dir("etag");
    let cache = DiskCache::new(dir.clone(), DEFAULT_CACHE_TTL, PINNED_CACHE_TTL);

    let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let url = "https://docs.rs/tokio/latest";

    assert!(cache
        .put_with_etag_at(url, "cached docs", false, Some("\"etag-v1\""), t0)
        .is_ok());

    // Fresh before TTL
    let fresh = cache.lookup_at(url, false, t0 + Duration::from_hours(1));
    assert!(fresh.is_some());
    if let Some(entry) = fresh {
        assert!(entry.is_fresh);
        assert_eq!(entry.content, "cached docs");
        assert_eq!(entry.etag.as_deref(), Some("\"etag-v1\""));
    }

    // Expired with ETag: still returned as stale for conditional If-None-Match validation
    let t_stale = t0 + Duration::from_hours(25);
    let stale = cache.lookup_at(url, false, t_stale);
    assert!(stale.is_some());
    if let Some(entry) = stale {
        assert!(!entry.is_fresh);
        assert_eq!(entry.content, "cached docs");
        assert_eq!(entry.etag.as_deref(), Some("\"etag-v1\""));
    }

    // After 304 Not Modified, refresh extends TTL with optional new ETag
    assert!(cache
        .refresh_at(url, false, Some("\"etag-v2\""), t_stale)
        .is_ok());
    let refreshed = cache.lookup_at(url, false, t_stale + Duration::from_hours(12));
    assert!(refreshed.is_some());
    if let Some(entry) = refreshed {
        assert!(entry.is_fresh);
        assert_eq!(entry.content, "cached docs");
        assert_eq!(entry.etag.as_deref(), Some("\"etag-v2\""));
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn disk_cache_stats_and_clear_report_and_reset_entries() {
    let dir = unique_temp_dir("stats-clear");
    let cache = DiskCache::new(dir.clone(), DEFAULT_CACHE_TTL, PINNED_CACHE_TTL);

    let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert!(cache
        .put_at(
            "https://docs.rs/fresh/latest",
            "fresh body",
            false,
            t0 + Duration::from_hours(20)
        )
        .is_ok());
    assert!(cache
        .put_at("https://docs.rs/stale/latest", "stale body", false, t0)
        .is_ok());
    assert!(fs::write(dir.join("corrupt00000000"), "not-json").is_ok());

    let stats = cache.stats_at(t0 + Duration::from_hours(25));
    assert_eq!(stats.total_entries, 3);
    assert_eq!(stats.fresh_entries, 1);
    assert_eq!(stats.expired_entries, 1);
    assert_eq!(stats.corrupt_entries, 1);
    assert!(stats.total_bytes > 0);

    let cleared = cache.clear();
    assert_eq!(cleared, 3);

    let after = cache.stats_at(t0 + Duration::from_hours(25));
    assert_eq!(after.total_entries, 0);
    assert_eq!(after.total_bytes, 0);

    let _ = fs::remove_dir_all(&dir);
}
