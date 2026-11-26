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

/// Extract a clean semver/version string from a lockfile or version specifier.
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

/// Extract a version ONLY when `raw` is an exact pin (such as `==3.1.0`, `=1.0.120`, or bare
/// `2.1.8` in `package.json`). Rejects floating lower-bound ranges (`>=`, `>`, `<`, `^`, `~`, `*`,
/// `,`, `||`) so manifests without a lockfile do not pin stale lower-bound documentation.
fn normalize_exact_pin(raw: &str, allow_bare: bool) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.contains(',')
        || trimmed.contains('*')
        || trimmed.contains("||")
        || trimmed.contains(' ')
        || trimmed.contains("://")
    {
        return None;
    }

    let value = if let Some(rest) = trimmed.strip_prefix("==") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix('=') {
        if rest.starts_with('=') {
            return None;
        }
        rest
    } else if allow_bare
        && trimmed
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == 'v' || c == 'V')
    {
        trimmed
    } else {
        return None;
    };

    if value.starts_with(['^', '~', '>', '<', '!']) {
        return None;
    }
    normalize_version_spec(value)
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

/// Outcome of looking up a dependency inside a manifest file.
enum DepMatch {
    NotFound,
    DeclaredUnpinned,
    Pinned(String),
}

impl DepMatch {
    const fn is_declared(&self) -> bool {
        !matches!(self, Self::NotFound)
    }

    fn into_pinned(self) -> Option<String> {
        match self {
            Self::Pinned(ver) => Some(ver),
            Self::NotFound | Self::DeclaredUnpinned => None,
        }
    }

    fn from_exact(exact: Option<String>) -> Self {
        match exact {
            Some(ver) => Self::Pinned(ver),
            None => Self::DeclaredUnpinned,
        }
    }
}

fn find_in_cargo_toml(content: &str, library: &str) -> DepMatch {
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
            let exact = extract_quoted_value(line).and_then(|v| normalize_exact_pin(v, false));
            return DepMatch::from_exact(exact);
        }
        if rhs.starts_with('{') {
            for part in rhs.trim_matches(['{', '}']).split(',') {
                let part = part.trim();
                if part.starts_with("version") {
                    let exact =
                        extract_quoted_value(part).and_then(|v| normalize_exact_pin(v, false));
                    return DepMatch::from_exact(exact);
                }
            }
            return DepMatch::DeclaredUnpinned;
        }
    }
    DepMatch::NotFound
}

fn resolve_from_cargo_toml(content: &str, library: &str) -> Option<String> {
    find_in_cargo_toml(content, library).into_pinned()
}

fn find_in_package_json(content: &str, library: &str) -> DepMatch {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(content) else {
        return DepMatch::NotFound;
    };
    for section in ["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(spec) = json
            .get(section)
            .and_then(|obj| obj.get(library))
            .and_then(serde_json::Value::as_str)
        {
            return DepMatch::from_exact(normalize_exact_pin(spec, true));
        }
    }
    DepMatch::NotFound
}

fn resolve_from_package_json(content: &str, library: &str) -> Option<String> {
    find_in_package_json(content, library).into_pinned()
}

fn resolve_from_pnpm_lock(content: &str, library: &str) -> Option<String> {
    let mut in_importers = false;
    let mut matched_indent: Option<usize> = None;

    for raw_line in content.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = raw_line.len() - raw_line.trim_start().len();

        if indent == 0 {
            in_importers = trimmed == "importers:";
            matched_indent = None;
            continue;
        }
        if !in_importers {
            continue;
        }

        if let Some(pkg_indent) = matched_indent {
            if indent <= pkg_indent {
                matched_indent = None;
            } else if let Some(rest) = trimmed.strip_prefix("version:") {
                let raw_ver = rest.trim().trim_matches(['\'', '"']);
                // Strip peer-dependency suffix such as `2.1.1(hono@4.13.9)`.
                let base_ver = raw_ver.split('(').next().unwrap_or(raw_ver).trim();
                if let Some(ver) = normalize_version_spec(base_ver) {
                    return Some(ver);
                }
            }
        }

        if let Some(key_raw) = trimmed.strip_suffix(':') {
            let key = key_raw.trim().trim_matches(['\'', '"']);
            if key == library {
                matched_indent = Some(indent);
            }
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

fn parse_pep508_requirement(req: &str, target_norm: &str) -> DepMatch {
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
        return DepMatch::NotFound;
    }

    let mut rest = trimmed[name_end..].trim();
    if rest.starts_with('[') {
        let Some(close) = rest.find(']') else {
            return DepMatch::DeclaredUnpinned;
        };
        rest = rest[close + 1..].trim();
    }
    let before_marker = rest.split(';').next().unwrap_or("").trim();
    DepMatch::from_exact(normalize_exact_pin(before_marker, false))
}

fn find_in_pyproject_toml(content: &str, library: &str) -> DepMatch {
    let target = normalize_pep503(library);

    // 1. Scan quoted PEP 508 requirement strings
    for raw_line in content.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        let mut rest = line;
        while let Some(start) = rest.find('"') {
            let after_start = &rest[start + 1..];
            let Some(end) = after_start.find('"') else {
                break;
            };
            let quoted = &after_start[..end];
            let found = parse_pep508_requirement(quoted, &target);
            if found.is_declared() {
                return found;
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
            let exact = extract_quoted_value(line).and_then(|v| normalize_exact_pin(v, true));
            return DepMatch::from_exact(exact);
        }
    }

    DepMatch::NotFound
}

fn find_in_requirements_txt(content: &str, library: &str) -> DepMatch {
    let target = normalize_pep503(library);
    for raw_line in content.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('-') {
            continue;
        }
        let found = parse_pep508_requirement(line, &target);
        if found.is_declared() {
            return found;
        }
    }
    DepMatch::NotFound
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

fn parse_go_mod_requires(content: &str) -> Vec<(String, String)> {
    let mut requires = Vec::new();
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
        if let Some(clean_ver) = normalize_version_spec(ver) {
            requires.push((module_path.to_string(), clean_ver));
        }
    }

    requires
}

fn go_module_short_name(module_path: &str) -> &str {
    let segments: Vec<&str> = module_path.split('/').collect();
    if let Some(last) = segments.last().copied() {
        // Major version suffix `/v2`, `/v3`, ... -> package name is the preceding segment.
        if last.starts_with('v')
            && last.len() >= 2
            && last[1..].chars().all(|c| c.is_ascii_digit())
            && segments.len() >= 2
        {
            return segments[segments.len() - 2];
        }
        return last;
    }
    module_path
}

/// Resolve a short Go package name (such as `"pflag"` or `"yaml"`) to its full Go module path
/// when `go.mod` in `project_dir` contains a unique matching dependency.
#[must_use]
pub fn resolve_go_short_name(project_dir: &Path, library: &str) -> Option<String> {
    if library.contains('/') || !project_dir.is_dir() {
        return None;
    }
    let go_mod = read_bounded_manifest(&project_dir.join("go.mod"))?;
    let target = library.to_ascii_lowercase();
    let mut matches: Vec<String> = parse_go_mod_requires(&go_mod)
        .into_iter()
        .map(|(path, _)| path)
        .filter(|path| go_module_short_name(path).eq_ignore_ascii_case(&target))
        .collect();
    matches.dedup();
    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

fn resolve_from_go_mod(content: &str, library: &str) -> Option<String> {
    let target_module = resolve_go_module(library).to_ascii_lowercase();
    let requires = parse_go_mod_requires(content);

    for (module_path, ver) in &requires {
        if module_path.eq_ignore_ascii_case(&target_module) {
            return Some(ver.clone());
        }
    }

    // Fallback: match unique short package name when `library` has no `/`
    if !library.contains('/') {
        let short_matches: Vec<&str> = requires
            .iter()
            .filter(|(path, _)| go_module_short_name(path).eq_ignore_ascii_case(library))
            .map(|(_, ver)| ver.as_str())
            .collect();
        if short_matches.len() == 1 {
            return Some(short_matches[0].to_string());
        }
    }

    None
}

fn project_mentions_library(project_dir: &Path, library: &str, ecosystem: Ecosystem) -> bool {
    if resolve_project_version(project_dir, library, ecosystem).is_some() {
        return true;
    }
    match ecosystem {
        Ecosystem::Rust => read_bounded_manifest(&project_dir.join("Cargo.toml"))
            .is_some_and(|c| find_in_cargo_toml(&c, library).is_declared()),
        Ecosystem::Npm => read_bounded_manifest(&project_dir.join("package.json"))
            .is_some_and(|c| find_in_package_json(&c, library).is_declared()),
        Ecosystem::Python => {
            read_bounded_manifest(&project_dir.join("pyproject.toml"))
                .is_some_and(|c| find_in_pyproject_toml(&c, library).is_declared())
                || read_bounded_manifest(&project_dir.join("requirements.txt"))
                    .is_some_and(|c| find_in_requirements_txt(&c, library).is_declared())
        }
        Ecosystem::Go => resolve_go_short_name(project_dir, library).is_some(),
    }
}

/// Infer the documentation ecosystem for `library` from the manifests present in `project_dir`.
///
/// First checks whether `library` is explicitly declared or locked in one of the project's manifests
/// (including non-exact version ranges), then falls back to the repository's sole ecosystem when
/// only one language manifest exists.
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
        if project_mentions_library(project_dir, library, eco) {
            return Some(eco);
        }
    }

    let mut present = Vec::new();
    if project_dir.join("uv.lock").is_file()
        || project_dir.join("pyproject.toml").is_file()
        || project_dir.join("requirements.txt").is_file()
    {
        present.push(Ecosystem::Python);
    }
    if project_dir.join("Cargo.lock").is_file() || project_dir.join("Cargo.toml").is_file() {
        present.push(Ecosystem::Rust);
    }
    if project_dir.join("pnpm-lock.yaml").is_file()
        || project_dir.join("package-lock.json").is_file()
        || project_dir.join("package.json").is_file()
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

/// Resolve the pinned or locked version of `library` from local project manifests inside
/// `project_dir` (`Cargo.lock`, `Cargo.toml`, `pnpm-lock.yaml`, `package-lock.json`,
/// `package.json`, `uv.lock`, `pyproject.toml`, `requirements.txt`, or `go.mod`).
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
            if let Some(lock) = read_bounded_manifest(&project_dir.join("pnpm-lock.yaml")) {
                if let Some(version) = resolve_from_pnpm_lock(&lock, library) {
                    return Some(ResolvedDependency {
                        version,
                        source_file: "pnpm-lock.yaml",
                    });
                }
            }
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
            if let Some(pyproject) = read_bounded_manifest(&project_dir.join("pyproject.toml")) {
                if let Some(version) = find_in_pyproject_toml(&pyproject, library).into_pinned() {
                    return Some(ResolvedDependency {
                        version,
                        source_file: "pyproject.toml",
                    });
                }
            }
            let reqs = read_bounded_manifest(&project_dir.join("requirements.txt"))?;
            let version = find_in_requirements_txt(&reqs, library).into_pinned()?;
            Some(ResolvedDependency {
                version,
                source_file: "requirements.txt",
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
