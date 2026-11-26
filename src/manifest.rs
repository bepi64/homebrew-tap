use std::fs;
use std::path::Path;

use crate::tools::{resolve_go_module, Ecosystem};

/// Maximum manifest or lockfile size read from disk (5 MiB) to avoid unbounded memory reads.
const MAX_MANIFEST_BYTES: u64 = 5 * 1024 * 1024;

/// Version resolved from a local project manifest or lockfile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDependency {
    pub version: String,
    pub source_file: &'static str,
}

fn read_bounded_manifest(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_MANIFEST_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// Extract a clean semver/version string from a dependency specifier such as
/// `^1.40.0`, `~3.24.2`, `>=2.9.2,<3.0`, `==3.1.0`, or `v1.10.0`.
/// Returns `None` for wildcards, workspace references, URLs, or empty specs.
#[must_use]
pub fn normalize_version_spec(raw: &str) -> Option<String> {
    let first = raw.split(',').next()?.trim();
    if first.is_empty() || first.contains("://") || first.starts_with("workspace:") {
        return None;
    }

    let stripped = first
        .trim_start_matches(['^', '~', '=', '>', '<', ' '])
        .trim();
    if stripped.is_empty()
        || stripped.eq_ignore_ascii_case("latest")
        || stripped.contains('*')
        || stripped.contains("..")
    {
        return None;
    }

    // Must start with an ASCII digit (or `v` followed by a digit, e.g. Go `v1.10.0`).
    let mut chars = stripped.chars();
    let starts_numeric = match (chars.next(), chars.next()) {
        (Some(c), _) if c.is_ascii_digit() => true,
        (Some('v' | 'V'), Some(c)) if c.is_ascii_digit() => true,
        _ => false,
    };
    if !starts_numeric {
        return None;
    }

    let candidate: String = stripped
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '+'))
        .collect();

    if candidate.is_empty() || candidate.contains("..") {
        return None;
    }
    Some(candidate)
}

fn normalize_crate_name(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('-', "_")
}

fn normalize_pep503(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_sep = false;
    for ch in name.trim().chars() {
        if matches!(ch, '-' | '_' | '.') {
            if !prev_sep {
                out.push('-');
                prev_sep = true;
            }
        } else {
            out.push(ch.to_ascii_lowercase());
            prev_sep = false;
        }
    }
    out
}

fn extract_quoted_value(line: &str) -> Option<&str> {
    let after_eq = line.split_once('=')?.1.trim();
    let quote = after_eq.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &after_eq[1..];
    let end = rest.find(quote)?;
    Some(&rest[..end])
}

fn resolve_from_cargo_lock(content: &str, library: &str) -> Option<String> {
    let target = normalize_crate_name(library);
    let mut in_package = false;
    let mut current_name: Option<String> = None;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line == "[[package]]" {
            in_package = true;
            current_name = None;
            continue;
        }
        if line.starts_with('[') {
            in_package = false;
            current_name = None;
            continue;
        }
        if !in_package {
            continue;
        }
        if line.starts_with("name") {
            if let Some(val) = extract_quoted_value(line) {
                current_name = Some(normalize_crate_name(val));
            }
        } else if line.starts_with("version") && current_name.as_deref() == Some(target.as_str()) {
            if let Some(val) = extract_quoted_value(line) {
                return normalize_version_spec(val);
            }
        }
    }
    None
}

fn resolve_from_cargo_toml(content: &str, library: &str) -> Option<String> {
    let target = normalize_crate_name(library);
    let mut in_deps = false;

    for raw_line in content.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let section = &line[1..line.len() - 1];
            in_deps = section == "dependencies"
                || section == "dev-dependencies"
                || section == "build-dependencies"
                || section == "workspace.dependencies"
                || section.ends_with(".dependencies");
            continue;
        }
        if !in_deps {
            continue;
        }
        let Some((key, rhs)) = line.split_once('=') else {
            continue;
        };
        if normalize_crate_name(key.trim().trim_matches('"')) != target {
            continue;
        }
        let rhs = rhs.trim();
        if rhs.starts_with('"') || rhs.starts_with('\'') {
            if let Some(val) = extract_quoted_value(line) {
                return normalize_version_spec(val);
            }
        } else if rhs.starts_with('{') {
            for part in rhs.trim_matches(['{', '}']).split(',') {
                let part = part.trim();
                if part.starts_with("version") {
                    if let Some(val) = extract_quoted_value(part) {
                        return normalize_version_spec(val);
                    }
                }
            }
        }
    }
    None
}

fn resolve_from_package_json(content: &str, library: &str) -> Option<String> {
    let json = serde_json::from_str::<serde_json::Value>(content).ok()?;
    for section in ["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(spec) = json
            .get(section)
            .and_then(|obj| obj.get(library))
            .and_then(serde_json::Value::as_str)
        {
            if let Some(ver) = normalize_version_spec(spec) {
                return Some(ver);
            }
        }
    }
    None
}

fn parse_pep508_requirement(req: &str, target_norm: &str) -> Option<String> {
    let trimmed = req.trim();
    let name_end = trimmed
        .find(|ch: char| {
            matches!(
                ch,
                '[' | '=' | '<' | '>' | '~' | '!' | ';' | ' ' | '\t' | '@'
            )
        })
        .unwrap_or(trimmed.len());
    let pkg_name = &trimmed[..name_end];
    if normalize_pep503(pkg_name) != target_norm {
        return None;
    }

    let mut rest = trimmed[name_end..].trim();
    if rest.starts_with('[') {
        let close = rest.find(']')?;
        rest = rest[close + 1..].trim();
    }
    let before_marker = rest.split(';').next()?.trim();
    normalize_version_spec(before_marker)
}

fn resolve_from_pyproject_toml(content: &str, library: &str) -> Option<String> {
    let target = normalize_pep503(library);

    // 1. Scan quoted PEP 508 requirement strings (covers [project].dependencies, optional-dependencies, dependency-groups)
    for raw_line in content.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        let mut rest = line;
        while let Some(start) = rest.find('"') {
            let after_start = &rest[start + 1..];
            let Some(end) = after_start.find('"') else {
                break;
            };
            let quoted = &after_start[..end];
            if let Some(ver) = parse_pep508_requirement(quoted, &target) {
                return Some(ver);
            }
            rest = &after_start[end + 1..];
        }
    }

    // 2. Fallback: Poetry-style key = "version" under [tool.poetry.dependencies]
    let mut in_poetry_deps = false;
    for raw_line in content.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_poetry_deps =
                line == "[tool.poetry.dependencies]" || line.starts_with("[tool.poetry.group.");
            continue;
        }
        if !in_poetry_deps {
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        if normalize_pep503(key) == target {
            if let Some(val) = extract_quoted_value(line) {
                return normalize_version_spec(val);
            }
        }
    }

    None
}

fn resolve_from_go_mod(content: &str, library: &str) -> Option<String> {
    let target_module = resolve_go_module(library).to_ascii_lowercase();
    let mut in_require_block = false;

    for raw_line in content.lines() {
        let line = raw_line.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("require (") || line == "require(" {
            in_require_block = true;
            continue;
        }
        if in_require_block && line == ")" {
            in_require_block = false;
            continue;
        }

        let spec = if in_require_block {
            line
        } else if let Some(rest) = line.strip_prefix("require ") {
            rest.trim()
        } else {
            continue;
        };

        let mut parts = spec.split_whitespace();
        let (Some(module_path), Some(ver)) = (parts.next(), parts.next()) else {
            continue;
        };
        if module_path.eq_ignore_ascii_case(&target_module) {
            return normalize_version_spec(ver);
        }
    }

    None
}

fn resolve_from_package_lock(content: &str, library: &str) -> Option<String> {
    let json = serde_json::from_str::<serde_json::Value>(content).ok()?;
    let node_modules_key = format!("node_modules/{library}");

    // Lockfile v2 / v3: `packages["node_modules/<pkg>"].version`
    if let Some(ver) = json
        .get("packages")
        .and_then(|pkgs| pkgs.get(&node_modules_key))
        .and_then(|entry| entry.get("version"))
        .and_then(serde_json::Value::as_str)
        .and_then(normalize_version_spec)
    {
        return Some(ver);
    }

    // Lockfile v1 fallback: `dependencies["<pkg>"].version`
    json.get("dependencies")
        .and_then(|deps| deps.get(library))
        .and_then(|entry| entry.get("version"))
        .and_then(serde_json::Value::as_str)
        .and_then(normalize_version_spec)
}

fn resolve_from_uv_lock(content: &str, library: &str) -> Option<String> {
    let target = normalize_pep503(library);
    let mut in_package = false;
    let mut current_name: Option<String> = None;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line == "[[package]]" {
            in_package = true;
            current_name = None;
            continue;
        }
        if line.starts_with('[') {
            in_package = false;
            current_name = None;
            continue;
        }
        if !in_package {
            continue;
        }
        if line.starts_with("name") {
            if let Some(val) = extract_quoted_value(line) {
                current_name = Some(normalize_pep503(val));
            }
        } else if line.starts_with("version") && current_name.as_deref() == Some(target.as_str()) {
            if let Some(val) = extract_quoted_value(line) {
                return normalize_version_spec(val);
            }
        }
    }
    None
}

/// Infer the documentation ecosystem for `library` from the manifests present in `project_dir`.
///
/// First checks whether `library` is explicitly declared/locked in one of the project's manifests
/// (resolving name collisions such as `ruff` in a Python repo vs the Rust crate `ruff`). If not
/// listed yet, falls back to the repository's sole ecosystem when only one language manifest exists.
#[must_use]
pub fn detect_project_ecosystem(project_dir: &Path, library: &str) -> Option<Ecosystem> {
    if !project_dir.is_dir() {
        return None;
    }

    for eco in [
        Ecosystem::Python,
        Ecosystem::Rust,
        Ecosystem::Npm,
        Ecosystem::Go,
    ] {
        if resolve_project_version(project_dir, library, eco).is_some() {
            return Some(eco);
        }
    }

    let mut present = Vec::new();
    if project_dir.join("uv.lock").is_file() || project_dir.join("pyproject.toml").is_file() {
        present.push(Ecosystem::Python);
    }
    if project_dir.join("Cargo.lock").is_file() || project_dir.join("Cargo.toml").is_file() {
        present.push(Ecosystem::Rust);
    }
    if project_dir.join("package-lock.json").is_file() || project_dir.join("package.json").is_file()
    {
        present.push(Ecosystem::Npm);
    }
    if project_dir.join("go.mod").is_file() {
        present.push(Ecosystem::Go);
    }

    if present.len() == 1 {
        present.into_iter().next()
    } else {
        None
    }
}

/// Resolve the pinned or declared version of `library` from local project manifests inside
/// `project_dir` (`Cargo.lock`, `Cargo.toml`, `package-lock.json`, `package.json`, `uv.lock`,
/// `pyproject.toml`, or `go.mod`).
#[must_use]
pub fn resolve_project_version(
    project_dir: &Path,
    library: &str,
    ecosystem: Ecosystem,
) -> Option<ResolvedDependency> {
    if !project_dir.is_dir() {
        return None;
    }

    match ecosystem {
        Ecosystem::Rust => {
            if let Some(lock) = read_bounded_manifest(&project_dir.join("Cargo.lock")) {
                if let Some(version) = resolve_from_cargo_lock(&lock, library) {
                    return Some(ResolvedDependency {
                        version,
                        source_file: "Cargo.lock",
                    });
                }
            }
            let toml = read_bounded_manifest(&project_dir.join("Cargo.toml"))?;
            let version = resolve_from_cargo_toml(&toml, library)?;
            Some(ResolvedDependency {
                version,
                source_file: "Cargo.toml",
            })
        }
        Ecosystem::Npm => {
            if let Some(lock) = read_bounded_manifest(&project_dir.join("package-lock.json")) {
                if let Some(version) = resolve_from_package_lock(&lock, library) {
                    return Some(ResolvedDependency {
                        version,
                        source_file: "package-lock.json",
                    });
                }
            }
            let pkg = read_bounded_manifest(&project_dir.join("package.json"))?;
            let version = resolve_from_package_json(&pkg, library)?;
            Some(ResolvedDependency {
                version,
                source_file: "package.json",
            })
        }
        Ecosystem::Python => {
            if let Some(lock) = read_bounded_manifest(&project_dir.join("uv.lock")) {
                if let Some(version) = resolve_from_uv_lock(&lock, library) {
                    return Some(ResolvedDependency {
                        version,
                        source_file: "uv.lock",
                    });
                }
            }
            let pyproject = read_bounded_manifest(&project_dir.join("pyproject.toml"))?;
            let version = resolve_from_pyproject_toml(&pyproject, library)?;
            Some(ResolvedDependency {
                version,
                source_file: "pyproject.toml",
            })
        }
        Ecosystem::Go => {
            let go_mod = read_bounded_manifest(&project_dir.join("go.mod"))?;
            let version = resolve_from_go_mod(&go_mod, library)?;
            Some(ResolvedDependency {
                version,
                source_file: "go.mod",
            })
        }
    }
}
