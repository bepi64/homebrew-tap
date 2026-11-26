use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use fast_docs::manifest::{normalize_version_spec, resolve_project_version};
use fast_docs::tools::Ecosystem;
use proptest::prelude::*;

fn unique_project_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!(
        "fast-docs-manifest-{label}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn resolves_rust_version_preferring_cargo_lock_over_cargo_toml() {
    let dir = unique_project_dir("rust");
    assert!(fs::create_dir_all(&dir).is_ok());

    let cargo_toml = r#"
[package]
name = "demo"
version = "0.1.0"

[dependencies]
tokio = { version = "1", features = ["full"] }
serde_json = "^1.0.120"
"#;
    let cargo_lock = r#"
version = 4

[[package]]
name = "tokio"
version = "1.54.1"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "serde_json"
version = "1.0.154"
"#;

    assert!(fs::write(dir.join("Cargo.toml"), cargo_toml).is_ok());
    assert!(fs::write(dir.join("Cargo.lock"), cargo_lock).is_ok());

    let tokio = resolve_project_version(&dir, "tokio", Ecosystem::Rust);
    assert!(tokio.is_some());
    if let Some(resolved) = tokio {
        assert_eq!(resolved.version, "1.54.1");
        assert_eq!(resolved.source_file, "Cargo.lock");
    }

    // Crate name hyphen/underscore normalization
    let serde_json = resolve_project_version(&dir, "serde-json", Ecosystem::Rust);
    assert!(serde_json.is_some());
    if let Some(resolved) = serde_json {
        assert_eq!(resolved.version, "1.0.154");
        assert_eq!(resolved.source_file, "Cargo.lock");
    }

    // Fallback to Cargo.toml when Cargo.lock is absent
    assert!(fs::remove_file(dir.join("Cargo.lock")).is_ok());
    let serde_from_toml = resolve_project_version(&dir, "serde_json", Ecosystem::Rust);
    assert!(serde_from_toml.is_some());
    if let Some(resolved) = serde_from_toml {
        assert_eq!(resolved.version, "1.0.120");
        assert_eq!(resolved.source_file, "Cargo.toml");
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resolves_npm_version_from_package_json_and_ignores_non_semver_specs() {
    let dir = unique_project_dir("npm");
    assert!(fs::create_dir_all(&dir).is_ok());

    let package_json = r#"{
  "name": "web-app",
  "dependencies": {
    "zod": "^3.24.2",
    "@modelcontextprotocol/sdk": "~1.32.2",
    "local-pkg": "workspace:*",
    "git-pkg": "github:user/repo"
  },
  "devDependencies": {
    "vitest": "2.1.8"
  }
}"#;
    assert!(fs::write(dir.join("package.json"), package_json).is_ok());

    let zod = resolve_project_version(&dir, "zod", Ecosystem::Npm);
    assert_eq!(zod.as_ref().map(|r| r.version.as_str()), Some("3.24.2"));
    assert_eq!(zod.as_ref().map(|r| r.source_file), Some("package.json"));

    let mcp = resolve_project_version(&dir, "@modelcontextprotocol/sdk", Ecosystem::Npm);
    assert_eq!(mcp.as_ref().map(|r| r.version.as_str()), Some("1.32.2"));

    let vitest = resolve_project_version(&dir, "vitest", Ecosystem::Npm);
    assert_eq!(vitest.as_ref().map(|r| r.version.as_str()), Some("2.1.8"));

    assert_eq!(
        resolve_project_version(&dir, "local-pkg", Ecosystem::Npm),
        None
    );
    assert_eq!(
        resolve_project_version(&dir, "git-pkg", Ecosystem::Npm),
        None
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resolves_npm_version_preferring_package_lock_over_package_json() {
    let dir = unique_project_dir("npm-lock");
    assert!(fs::create_dir_all(&dir).is_ok());

    let package_json = r#"{
  "dependencies": {
    "zod": "^3.20.0",
    "@modelcontextprotocol/sdk": "^1.0.0"
  }
}"#;
    let package_lock = r#"{
  "name": "web-app",
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "web-app" },
    "node_modules/zod": { "version": "3.24.4" },
    "node_modules/@modelcontextprotocol/sdk": { "version": "1.32.2" }
  }
}"#;
    assert!(fs::write(dir.join("package.json"), package_json).is_ok());
    assert!(fs::write(dir.join("package-lock.json"), package_lock).is_ok());

    let zod = resolve_project_version(&dir, "zod", Ecosystem::Npm);
    assert_eq!(zod.as_ref().map(|r| r.version.as_str()), Some("3.24.4"));
    assert_eq!(
        zod.as_ref().map(|r| r.source_file),
        Some("package-lock.json")
    );

    let mcp = resolve_project_version(&dir, "@modelcontextprotocol/sdk", Ecosystem::Npm);
    assert_eq!(mcp.as_ref().map(|r| r.version.as_str()), Some("1.32.2"));
    assert_eq!(
        mcp.as_ref().map(|r| r.source_file),
        Some("package-lock.json")
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resolves_python_version_preferring_uv_lock_over_pyproject_toml() {
    let dir = unique_project_dir("uv-lock");
    assert!(fs::create_dir_all(&dir).is_ok());

    let pyproject = r#"
[project]
dependencies = ["httpx>=0.27.0", "scikit-learn>=1.5.0"]
"#;
    let uv_lock = r#"
version = 1

[[package]]
name = "httpx"
version = "0.28.1"

[[package]]
name = "httpx2"
version = "2.16.0"

[[package]]
name = "scikit-learn"
version = "1.6.2"
"#;
    assert!(fs::write(dir.join("pyproject.toml"), pyproject).is_ok());
    assert!(fs::write(dir.join("uv.lock"), uv_lock).is_ok());

    let httpx = resolve_project_version(&dir, "httpx", Ecosystem::Python);
    assert_eq!(httpx.as_ref().map(|r| r.version.as_str()), Some("0.28.1"));
    assert_eq!(httpx.as_ref().map(|r| r.source_file), Some("uv.lock"));

    let sklearn = resolve_project_version(&dir, "scikit_learn", Ecosystem::Python);
    assert_eq!(sklearn.as_ref().map(|r| r.version.as_str()), Some("1.6.2"));
    assert_eq!(sklearn.as_ref().map(|r| r.source_file), Some("uv.lock"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resolves_python_version_from_pyproject_toml_with_pep503_normalization() {
    let dir = unique_project_dir("python");
    assert!(fs::create_dir_all(&dir).is_ok());

    let pyproject = r#"
[project]
name = "app"
dependencies = [
    "flask==3.1.0",
    "pydantic>=2.9.2,<3.0",
    "scikit_learn~=1.5.1",
    "requests",
]
"#;
    assert!(fs::write(dir.join("pyproject.toml"), pyproject).is_ok());

    let flask = resolve_project_version(&dir, "Flask", Ecosystem::Python);
    assert_eq!(flask.as_ref().map(|r| r.version.as_str()), Some("3.1.0"));
    assert_eq!(
        flask.as_ref().map(|r| r.source_file),
        Some("pyproject.toml")
    );

    let pydantic = resolve_project_version(&dir, "pydantic", Ecosystem::Python);
    assert_eq!(pydantic.as_ref().map(|r| r.version.as_str()), Some("2.9.2"));

    let sklearn = resolve_project_version(&dir, "scikit-learn", Ecosystem::Python);
    assert_eq!(sklearn.as_ref().map(|r| r.version.as_str()), Some("1.5.1"));

    // Unpinned dependency without version returns None (falls back to latest)
    assert_eq!(
        resolve_project_version(&dir, "requests", Ecosystem::Python),
        None
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resolves_go_version_from_go_mod_including_aliases() {
    let dir = unique_project_dir("go");
    assert!(fs::create_dir_all(&dir).is_ok());

    let go_mod = r"
module example.com/myservice

go 1.23

require github.com/spf13/cobra v1.8.1

require (
    github.com/gin-gonic/gin v1.10.0
    go.uber.org/zap v1.27.0 // indirect
)
";
    assert!(fs::write(dir.join("go.mod"), go_mod).is_ok());

    let gin = resolve_project_version(&dir, "gin", Ecosystem::Go);
    assert_eq!(gin.as_ref().map(|r| r.version.as_str()), Some("v1.10.0"));
    assert_eq!(gin.as_ref().map(|r| r.source_file), Some("go.mod"));

    let cobra = resolve_project_version(&dir, "github.com/spf13/cobra", Ecosystem::Go);
    assert_eq!(cobra.as_ref().map(|r| r.version.as_str()), Some("v1.8.1"));

    let zap = resolve_project_version(&dir, "zap", Ecosystem::Go);
    assert_eq!(zap.as_ref().map(|r| r.version.as_str()), Some("v1.27.0"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn returns_none_on_missing_dir_or_malformed_manifests() {
    let missing = unique_project_dir("does-not-exist");
    assert_eq!(
        resolve_project_version(&missing, "tokio", Ecosystem::Rust),
        None
    );

    let dir = unique_project_dir("malformed");
    assert!(fs::create_dir_all(&dir).is_ok());
    assert!(fs::write(dir.join("package.json"), "{not valid json").is_ok());
    assert_eq!(resolve_project_version(&dir, "zod", Ecosystem::Npm), None);

    let _ = fs::remove_dir_all(&dir);
}

proptest! {
    #[test]
    fn normalize_version_spec_never_panics_and_outputs_safe_version_chars(raw in ".{0,80}") {
        if let Some(ver) = normalize_version_spec(&raw) {
            prop_assert!(!ver.is_empty());
            prop_assert!(!ver.contains(".."));
            prop_assert!(ver.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+')));
        }
    }
}
