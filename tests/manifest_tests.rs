use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use fast_docs::manifest::{
    detect_project_ecosystem, normalize_version_spec, resolve_go_short_name,
    resolve_project_version,
};
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
fn resolves_rust_version_preferring_cargo_lock_and_exact_cargo_toml_pins() {
    let dir = unique_project_dir("rust");
    assert!(fs::create_dir_all(&dir).is_ok());

    let cargo_toml = r#"
[package]
name = "demo"
version = "0.1.0"

[dependencies]
tokio = { version = "1", features = ["full"] }
serde_json = "=1.0.120"
regex = "^1.10.0"
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
    assert_eq!(tokio.as_ref().map(|r| r.version.as_str()), Some("1.54.1"));
    assert_eq!(tokio.as_ref().map(|r| r.source_file), Some("Cargo.lock"));

    // Crate name hyphen/underscore normalization
    let serde_json = resolve_project_version(&dir, "serde-json", Ecosystem::Rust);
    assert_eq!(
        serde_json.as_ref().map(|r| r.version.as_str()),
        Some("1.0.154")
    );
    assert_eq!(
        serde_json.as_ref().map(|r| r.source_file),
        Some("Cargo.lock")
    );

    // Without Cargo.lock: exact `=1.0.120` pin resolves, floating `^1.10.0` falls back to None (latest)
    assert!(fs::remove_file(dir.join("Cargo.lock")).is_ok());
    let serde_from_toml = resolve_project_version(&dir, "serde_json", Ecosystem::Rust);
    assert_eq!(
        serde_from_toml.as_ref().map(|r| r.version.as_str()),
        Some("1.0.120")
    );
    assert_eq!(
        serde_from_toml.as_ref().map(|r| r.source_file),
        Some("Cargo.toml")
    );
    assert_eq!(
        resolve_project_version(&dir, "regex", Ecosystem::Rust),
        None
    );
    assert_eq!(
        detect_project_ecosystem(&dir, "regex"),
        Some(Ecosystem::Rust)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resolves_npm_exact_package_json_and_ignores_ranges_without_lockfile() {
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

    // Caret/tilde ranges without lockfile must NOT pin an old lower bound
    assert_eq!(resolve_project_version(&dir, "zod", Ecosystem::Npm), None);
    assert_eq!(
        resolve_project_version(&dir, "@modelcontextprotocol/sdk", Ecosystem::Npm),
        None
    );
    // Exact pin without range operator resolves cleanly
    let vitest = resolve_project_version(&dir, "vitest", Ecosystem::Npm);
    assert_eq!(vitest.as_ref().map(|r| r.version.as_str()), Some("2.1.8"));
    assert_eq!(vitest.as_ref().map(|r| r.source_file), Some("package.json"));

    // Ecosystem detection still recognizes range-specified deps in package.json
    assert_eq!(detect_project_ecosystem(&dir, "zod"), Some(Ecosystem::Npm));

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
fn resolves_npm_version_from_pnpm_lock_and_package_lock() {
    let dir = unique_project_dir("npm-locks");
    assert!(fs::create_dir_all(&dir).is_ok());

    let package_json = r#"{
  "dependencies": {
    "zod": "^3.23.8",
    "@hono/node-server": "^2.0.12"
  }
}"#;
    let pnpm_lock = r"lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      '@hono/node-server':
        specifier: ^2.0.12
        version: 2.1.1(hono@4.13.9)
      zod:
        specifier: ^3.23.8
        version: 3.25.76
packages:
  zod@3.25.76:
    resolution: {integrity: sha512-abc}
";
    let package_lock = r#"{
  "name": "web-app",
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "web-app" },
    "node_modules/zod": { "version": "3.24.4" }
  }
}"#;

    assert!(fs::write(dir.join("package.json"), package_json).is_ok());
    assert!(fs::write(dir.join("pnpm-lock.yaml"), pnpm_lock).is_ok());
    assert!(fs::write(dir.join("package-lock.json"), package_lock).is_ok());

    // pnpm-lock.yaml is preferred when present and strips peer-dep suffixes
    let zod = resolve_project_version(&dir, "zod", Ecosystem::Npm);
    assert_eq!(zod.as_ref().map(|r| r.version.as_str()), Some("3.25.76"));
    assert_eq!(zod.as_ref().map(|r| r.source_file), Some("pnpm-lock.yaml"));

    let node_srv = resolve_project_version(&dir, "@hono/node-server", Ecosystem::Npm);
    assert_eq!(node_srv.as_ref().map(|r| r.version.as_str()), Some("2.1.1"));
    assert_eq!(
        node_srv.as_ref().map(|r| r.source_file),
        Some("pnpm-lock.yaml")
    );

    // Falls back to package-lock.json when pnpm-lock.yaml is absent
    assert!(fs::remove_file(dir.join("pnpm-lock.yaml")).is_ok());
    let zod_npm_lock = resolve_project_version(&dir, "zod", Ecosystem::Npm);
    assert_eq!(
        zod_npm_lock.as_ref().map(|r| r.version.as_str()),
        Some("3.24.4")
    );
    assert_eq!(
        zod_npm_lock.as_ref().map(|r| r.source_file),
        Some("package-lock.json")
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resolves_python_version_from_uv_lock_pyproject_and_requirements_txt() {
    let dir = unique_project_dir("python-all");
    assert!(fs::create_dir_all(&dir).is_ok());

    let pyproject = r#"
[project]
name = "app"
dependencies = [
    "flask==3.1.0",
    "rich>=10,<15",
    "scikit_learn~=1.5.1",
    "requests",
]
"#;
    let requirements = "\
# Dev tools
pytest==8.4.1
ruff==0.12.11
httpx>=0.27.0
";
    let uv_lock = r#"
version = 1

[[package]]
name = "httpx"
version = "0.28.1"

[[package]]
name = "scikit-learn"
version = "1.6.2"
"#;
    assert!(fs::write(dir.join("pyproject.toml"), pyproject).is_ok());
    assert!(fs::write(dir.join("requirements.txt"), requirements).is_ok());
    assert!(fs::write(dir.join("uv.lock"), uv_lock).is_ok());

    // 1. uv.lock wins for locked packages
    let httpx = resolve_project_version(&dir, "httpx", Ecosystem::Python);
    assert_eq!(httpx.as_ref().map(|r| r.version.as_str()), Some("0.28.1"));
    assert_eq!(httpx.as_ref().map(|r| r.source_file), Some("uv.lock"));

    let sklearn = resolve_project_version(&dir, "scikit_learn", Ecosystem::Python);
    assert_eq!(sklearn.as_ref().map(|r| r.version.as_str()), Some("1.6.2"));
    assert_eq!(sklearn.as_ref().map(|r| r.source_file), Some("uv.lock"));

    // 2. Exact == pin in pyproject.toml resolves; lower-bound range rich>=10,<15 does NOT pin 10
    let flask = resolve_project_version(&dir, "Flask", Ecosystem::Python);
    assert_eq!(flask.as_ref().map(|r| r.version.as_str()), Some("3.1.0"));
    assert_eq!(
        flask.as_ref().map(|r| r.source_file),
        Some("pyproject.toml")
    );
    assert_eq!(
        resolve_project_version(&dir, "rich", Ecosystem::Python),
        None
    );

    // 3. Exact == pins in requirements.txt resolve
    let ruff = resolve_project_version(&dir, "ruff", Ecosystem::Python);
    assert_eq!(ruff.as_ref().map(|r| r.version.as_str()), Some("0.12.11"));
    assert_eq!(
        ruff.as_ref().map(|r| r.source_file),
        Some("requirements.txt")
    );

    let pytest = resolve_project_version(&dir, "pytest", Ecosystem::Python);
    assert_eq!(pytest.as_ref().map(|r| r.version.as_str()), Some("8.4.1"));
    assert_eq!(
        pytest.as_ref().map(|r| r.source_file),
        Some("requirements.txt")
    );

    // Ecosystem detection recognizes requirements.txt even when pyproject.toml has only ranges
    assert_eq!(
        detect_project_ecosystem(&dir, "rich"),
        Some(Ecosystem::Python)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resolves_go_version_and_short_name_from_go_mod() {
    let dir = unique_project_dir("go");
    assert!(fs::create_dir_all(&dir).is_ok());

    let go_mod = r"
module example.com/myservice

go 1.23

require github.com/spf13/cobra v1.8.1

require (
    github.com/spf13/pflag v1.0.9
    github.com/gin-gonic/gin v1.10.0
    go.yaml.in/yaml/v3 v3.0.4
)
";
    assert!(fs::write(dir.join("go.mod"), go_mod).is_ok());

    let gin = resolve_project_version(&dir, "gin", Ecosystem::Go);
    assert_eq!(gin.as_ref().map(|r| r.version.as_str()), Some("v1.10.0"));
    assert_eq!(gin.as_ref().map(|r| r.source_file), Some("go.mod"));

    // Short name resolution from go.mod
    assert_eq!(
        resolve_go_short_name(&dir, "pflag").as_deref(),
        Some("github.com/spf13/pflag")
    );
    assert_eq!(
        resolve_go_short_name(&dir, "yaml").as_deref(),
        Some("go.yaml.in/yaml/v3")
    );
    let pflag = resolve_project_version(&dir, "pflag", Ecosystem::Go);
    assert_eq!(pflag.as_ref().map(|r| r.version.as_str()), Some("v1.0.9"));

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
