---
paths:
  - "**/.github/workflows/**"
  - "**/.github/actions/**"
  - "**/dependabot.yml"
  - "**/dependabot.yaml"
---

# GitHub Actions

Pin every action to a full commit SHA with the version in a trailing comment:

```yaml
- uses: actions/checkout@<full-sha>  # vX.Y.Z
  with:
    persist-credentials: false
```

Look up the current stable version and its SHA—never carry either from memory.

## Before Committing

```bash
actionlint                       # syntax and expression errors
zizmor .github/workflows/        # security audit
```

## Hardening

- Set `permissions:` explicitly at the workflow or job level; default to `contents: read`.
- Set `persist-credentials: false` on `actions/checkout` unless the job genuinely needs to push.
- Never interpolate untrusted input (`github.event.issue.title`, branch names, PR bodies) directly into a `run:` block—pass it through `env:` and reference the variable.
- Prefer `pull_request` over `pull_request_target`; the latter runs with access to secrets and a read/write `GITHUB_TOKEN`.

## Dependabot

Configure 7-day cooldowns and grouped updates. Use the `cargo` ecosystem for Rust crates (`Cargo.toml` / `Cargo.lock`) and the `github-actions` ecosystem for workflow actions.
