use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use fast_docs::local_docs::search_local_docs;

fn unique_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!(
        "fast-docs-local-{label}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn search_local_docs_indexes_markdown_ranks_by_topic_and_paginates() {
    let dir = unique_dir("rank");
    assert!(fs::create_dir_all(dir.join("docs")).is_ok());

    assert!(fs::write(
        dir.join("README.md"),
        "# Project Overview\nBasic introduction to our internal service.\n"
    )
    .is_ok());
    assert!(fs::write(
        dir.join("docs/auth.md"),
        "# Authentication Flow\nUse OIDC JWT bearer tokens validated against JWKS.\n```rust\nlet claims = verify_jwt(token)?;\n```\n"
    )
    .is_ok());
    assert!(fs::write(
        dir.join("docs/deploy.md"),
        "# Deployment Guide\nDeploy containers using Helm charts on Kubernetes.\n"
    )
    .is_ok());

    let out = search_local_docs(&dir, Some("oidc jwt"), 2000, 0);
    assert!(out.is_ok());
    if let Ok(text) = out {
        assert!(text.contains("TITLE: Authentication Flow"));
        assert!(text.contains("SOURCE: docs/auth.md"));
        assert!(text.contains("verify_jwt"));
        assert!(!text.contains("TITLE: Deployment Guide"));
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn search_local_docs_excludes_secrets_ignored_dirs_and_escaping_symlinks() {
    let dir = unique_dir("security");
    let outside = unique_dir("outside");
    assert!(fs::create_dir_all(dir.join("docs")).is_ok());
    assert!(fs::create_dir_all(dir.join(".git")).is_ok());
    assert!(fs::create_dir_all(dir.join("target")).is_ok());
    assert!(fs::create_dir_all(dir.join("node_modules/pkg")).is_ok());
    assert!(fs::create_dir_all(&outside).is_ok());

    // Legitimate doc
    assert!(fs::write(
        dir.join("docs/guide.md"),
        "# Safe Guide\nPublic internal engineering guide.\n"
    )
    .is_ok());

    // Sensitive & excluded files that must NEVER be read
    assert!(fs::write(dir.join(".env"), "SECRET_TOKEN=super_secret_env\n").is_ok());
    assert!(fs::write(
        dir.join("docs/.env.md"),
        "SECRET_ENV_MD=should_never_leak\n"
    )
    .is_ok());
    assert!(fs::write(dir.join("docs/private.pem"), "PRIVATE KEY LEAK\n").is_ok());
    assert!(fs::write(dir.join(".git/README.md"), "GIT_INTERNAL_SECRET\n").is_ok());
    assert!(fs::write(dir.join("target/README.md"), "TARGET_ARTIFACT_NOISE\n").is_ok());
    assert!(fs::write(
        dir.join("node_modules/pkg/README.md"),
        "NODE_MODULES_NOISE\n"
    )
    .is_ok());

    // Symlink escaping outside project_dir
    let outside_secret = outside.join("secret.md");
    assert!(fs::write(&outside_secret, "# Outside Secret\nESCAPED_SYMLINK_LEAK\n").is_ok());
    let _ = symlink(&outside_secret, dir.join("docs/escape.md"));

    let out = search_local_docs(&dir, None, 10_000, 0);
    assert!(out.is_ok());
    if let Ok(text) = out {
        assert!(text.contains("TITLE: Safe Guide"));
        assert!(!text.contains("super_secret_env"));
        assert!(!text.contains("should_never_leak"));
        assert!(!text.contains("PRIVATE KEY LEAK"));
        assert!(!text.contains("GIT_INTERNAL_SECRET"));
        assert!(!text.contains("TARGET_ARTIFACT_NOISE"));
        assert!(!text.contains("NODE_MODULES_NOISE"));
        assert!(!text.contains("ESCAPED_SYMLINK_LEAK"));
    }

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&outside);
}

#[test]
fn search_local_docs_rejects_missing_directory() {
    let missing = unique_dir("missing");
    assert!(search_local_docs(&missing, None, 2000, 0).is_err());
}
