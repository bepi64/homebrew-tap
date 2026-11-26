use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Freshness window for mutable (`latest`) documentation entries: 24 hours.
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_hours(24);

/// Freshness window for explicitly version-pinned documentation entries: 7 days.
pub const PINNED_CACHE_TTL: Duration = Duration::from_hours(7 * 24);

/// Freshness window for cached `search_docs` query results: 15 minutes.
pub const SEARCH_CACHE_TTL: Duration = Duration::from_mins(15);

/// Current on-disk envelope format version.
const CACHE_SCHEMA_VERSION: u8 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    version: u8,
    fetched_at_secs: u64,
    pinned: bool,
    #[serde(default)]
    etag: Option<String>,
    content: String,
}

/// Result of a cache lookup, including freshness state and optional HTTP `ETag`
/// for RFC 9111 §4.3 conditional revalidation (`If-None-Match`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedValue {
    pub content: String,
    pub etag: Option<String>,
    pub is_fresh: bool,
}

/// Summary statistics of the local on-disk cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheStats {
    pub root: PathBuf,
    pub total_entries: usize,
    pub fresh_entries: usize,
    pub expired_entries: usize,
    pub corrupt_entries: usize,
    pub total_bytes: u64,
}

/// Pure-Rust local disk cache storing formatted documentation snippets under
/// SHA-256-derived filenames with explicit TTL expiration and `ETag` revalidation.
#[derive(Debug, Clone)]
pub struct DiskCache {
    root: PathBuf,
    default_ttl: Duration,
    pinned_ttl: Duration,
}

/// Derive a deterministic, path-safe 32-hex-character cache filename from a URL or key.
#[must_use]
pub fn cache_key(url: &str) -> String {
    let hash = Sha256::digest(format!("c7v2:{url}").as_bytes());
    hex::encode(&hash[..16])
}

fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

impl DiskCache {
    /// Create a disk cache rooted at `root` with explicit TTLs for mutable and pinned entries.
    #[must_use]
    pub const fn new(root: PathBuf, default_ttl: Duration, pinned_ttl: Duration) -> Self {
        Self {
            root,
            default_ttl,
            pinned_ttl,
        }
    }

    /// Resolve the default per-user cache directory (`$HOME/.cache/fast-docs/indexes`).
    #[must_use]
    pub fn default_user_cache() -> Option<Self> {
        let home = std::env::var("HOME").ok()?;
        let root = PathBuf::from(home)
            .join(".cache")
            .join("fast-docs")
            .join("indexes");
        Some(Self::new(root, DEFAULT_CACHE_TTL, PINNED_CACHE_TTL))
    }

    /// Read a fresh cached entry using the current wall-clock time.
    #[must_use]
    pub fn get(&self, url: &str, pinned: bool) -> Option<String> {
        self.get_at(url, pinned, SystemTime::now())
    }

    /// Read a fresh cached entry evaluated against a custom TTL.
    #[must_use]
    pub fn get_with_ttl(&self, key: &str, ttl: Duration) -> Option<String> {
        let path = self.root.join(cache_key(key));
        let raw = fs::read_to_string(&path).ok()?;
        let Ok(entry) = serde_json::from_str::<CacheEntry>(&raw) else {
            let _ = fs::remove_file(&path);
            return None;
        };
        if entry.version != CACHE_SCHEMA_VERSION {
            let _ = fs::remove_file(&path);
            return None;
        }
        let now_secs = unix_secs(SystemTime::now());
        if now_secs.saturating_sub(entry.fetched_at_secs) > ttl.as_secs() {
            let _ = fs::remove_file(&path);
            return None;
        }
        Some(entry.content)
    }

    /// Read a fresh cached entry evaluated against an explicit timestamp `now`.
    #[must_use]
    pub fn get_at(&self, url: &str, pinned: bool, now: SystemTime) -> Option<String> {
        self.lookup_at(url, pinned, now)
            .filter(|value| value.is_fresh)
            .map(|value| value.content)
    }

    /// Look up a cached entry using the current wall-clock time, returning stale entries
    /// that carry an `ETag` so callers can issue conditional `If-None-Match` requests.
    #[must_use]
    pub fn lookup(&self, url: &str, pinned: bool) -> Option<CachedValue> {
        self.lookup_at(url, pinned, SystemTime::now())
    }

    /// Look up a cached entry against an explicit timestamp `now`.
    #[must_use]
    pub fn lookup_at(&self, url: &str, pinned: bool, now: SystemTime) -> Option<CachedValue> {
        let path = self.root.join(cache_key(url));
        let raw = fs::read_to_string(&path).ok()?;
        let Ok(entry) = serde_json::from_str::<CacheEntry>(&raw) else {
            let _ = fs::remove_file(&path);
            return None;
        };

        if entry.version != CACHE_SCHEMA_VERSION {
            let _ = fs::remove_file(&path);
            return None;
        }

        let ttl = if pinned || entry.pinned {
            self.pinned_ttl
        } else {
            self.default_ttl
        };
        let now_secs = unix_secs(now);
        let is_fresh = now_secs.saturating_sub(entry.fetched_at_secs) <= ttl.as_secs();

        if !is_fresh && entry.etag.is_none() {
            let _ = fs::remove_file(&path);
            return None;
        }

        Some(CachedValue {
            content: entry.content,
            etag: entry.etag,
            is_fresh,
        })
    }

    /// Write a cache entry using the current wall-clock time.
    ///
    /// # Errors
    /// Returns an I/O error if the cache directory cannot be created or the file cannot be written.
    pub fn put(&self, url: &str, content: &str, pinned: bool) -> io::Result<()> {
        self.put_with_etag_at(url, content, pinned, None, SystemTime::now())
    }

    /// Write a cache entry with an optional HTTP `ETag` using the current wall-clock time.
    ///
    /// # Errors
    /// Returns an I/O error if directory creation, serialization, or atomic write fails.
    pub fn put_with_etag(
        &self,
        url: &str,
        content: &str,
        pinned: bool,
        etag: Option<&str>,
    ) -> io::Result<()> {
        self.put_with_etag_at(url, content, pinned, etag, SystemTime::now())
    }

    /// Atomically write a cache entry with an explicit timestamp `now`.
    ///
    /// # Errors
    /// Returns an I/O error if directory creation, serialization, temp-file writing, or rename fails.
    pub fn put_at(
        &self,
        url: &str,
        content: &str,
        pinned: bool,
        now: SystemTime,
    ) -> io::Result<()> {
        self.put_with_etag_at(url, content, pinned, None, now)
    }

    /// Atomically write a cache entry with an optional `ETag` and explicit timestamp `now`.
    ///
    /// # Errors
    /// Returns an I/O error if directory creation, serialization, temp-file writing, or rename fails.
    pub fn put_with_etag_at(
        &self,
        url: &str,
        content: &str,
        pinned: bool,
        etag: Option<&str>,
        now: SystemTime,
    ) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        let key = cache_key(url);
        let final_path = self.root.join(&key);
        let temp_path = self.root.join(format!("{key}.{}.tmp", std::process::id()));

        let entry = CacheEntry {
            version: CACHE_SCHEMA_VERSION,
            fetched_at_secs: unix_secs(now),
            pinned,
            etag: etag.map(ToOwned::to_owned),
            content: content.to_string(),
        };
        let payload = serde_json::to_string(&entry)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

        fs::write(&temp_path, payload)?;
        fs::rename(&temp_path, &final_path)?;
        Ok(())
    }

    /// Refresh an existing cached entry's timestamp (and optionally its `ETag`) after a
    /// `304 Not Modified` revalidation response.
    ///
    /// # Errors
    /// Returns an I/O error if the entry is missing, unreadable, or cannot be rewritten.
    pub fn refresh(&self, url: &str, pinned: bool, new_etag: Option<&str>) -> io::Result<()> {
        self.refresh_at(url, pinned, new_etag, SystemTime::now())
    }

    /// Refresh an existing cached entry at explicit timestamp `now`.
    ///
    /// # Errors
    /// Returns an I/O error if the entry is missing, invalid, or cannot be rewritten.
    pub fn refresh_at(
        &self,
        url: &str,
        pinned: bool,
        new_etag: Option<&str>,
        now: SystemTime,
    ) -> io::Result<()> {
        let path = self.root.join(cache_key(url));
        let raw = fs::read_to_string(&path)?;
        let entry = serde_json::from_str::<CacheEntry>(&raw)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        let etag = new_etag.map(ToOwned::to_owned).or(entry.etag);
        self.put_with_etag_at(
            url,
            &entry.content,
            pinned || entry.pinned,
            etag.as_deref(),
            now,
        )
    }

    /// Scan the cache directory and delete expired or unreadable entries.
    /// Returns the number of files removed.
    #[must_use]
    pub fn prune_expired(&self, now: SystemTime) -> usize {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return 0;
        };

        let now_secs = unix_secs(now);
        let mut removed = 0;

        for dir_entry in entries.flatten() {
            let path = dir_entry.path();
            if !path.is_file() {
                continue;
            }

            let is_stale = match fs::read_to_string(&path) {
                Ok(raw) => match serde_json::from_str::<CacheEntry>(&raw) {
                    Ok(entry) if entry.version == CACHE_SCHEMA_VERSION => {
                        let ttl = if entry.pinned {
                            self.pinned_ttl
                        } else {
                            self.default_ttl
                        };
                        now_secs.saturating_sub(entry.fetched_at_secs) > ttl.as_secs()
                    }
                    Ok(_) | Err(_) => true,
                },
                Err(_) => true,
            };

            if is_stale && fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }

        removed
    }

    /// Collect cache occupancy and freshness statistics using the current wall-clock time.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        self.stats_at(SystemTime::now())
    }

    /// Collect cache occupancy and freshness statistics evaluated against timestamp `now`.
    #[must_use]
    pub fn stats_at(&self, now: SystemTime) -> CacheStats {
        let mut stats = CacheStats {
            root: self.root.clone(),
            total_entries: 0,
            fresh_entries: 0,
            expired_entries: 0,
            corrupt_entries: 0,
            total_bytes: 0,
        };

        let Ok(entries) = fs::read_dir(&self.root) else {
            return stats;
        };

        let now_secs = unix_secs(now);
        for dir_entry in entries.flatten() {
            let path = dir_entry.path();
            if !path.is_file() {
                continue;
            }
            stats.total_entries += 1;
            if let Ok(meta) = dir_entry.metadata() {
                stats.total_bytes = stats.total_bytes.saturating_add(meta.len());
            }

            match fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<CacheEntry>(&raw).ok())
            {
                Some(entry) if entry.version == CACHE_SCHEMA_VERSION => {
                    let ttl = if entry.pinned {
                        self.pinned_ttl
                    } else {
                        self.default_ttl
                    };
                    if now_secs.saturating_sub(entry.fetched_at_secs) <= ttl.as_secs() {
                        stats.fresh_entries += 1;
                    } else {
                        stats.expired_entries += 1;
                    }
                }
                Some(_) | None => stats.corrupt_entries += 1,
            }
        }

        stats
    }

    /// Delete every regular file inside the cache root directory.
    /// Returns the number of files removed.
    #[must_use]
    pub fn clear(&self) -> usize {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return 0;
        };

        let mut removed = 0;
        for dir_entry in entries.flatten() {
            let path = dir_entry.path();
            if path.is_file() && fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}
