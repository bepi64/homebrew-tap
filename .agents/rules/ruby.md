---
paths:
  - "**/*.rb"
  - "**/Formula/**/*.rb"
  - "**/Casks/**/*.rb"
  - "**/Gemfile"
  - "**/Gemfile.lock"
---

# Ruby (Homebrew Tap)

**Runtime:** Homebrew's bundled Ruby for tap formulae and casks; latest stable Ruby (via `rbenv`, `mise`, or `asdf`) for standalone Ruby scripts.

| Purpose | Tool |
| --- | --- |
| Tap style & lint | `brew style <path>` (`rubocop` for standalone Ruby) |
| Format / fix | `brew style --fix <path>` (`rubocop -A` for standalone Ruby) |
| Formula audit | `brew audit --strict --online <formula>` (`--new` for new formulae) |
| Test | `brew test <formula>` (`bundle exec rspec` or `minitest` for standalone Ruby) |
| Supply chain | Pinned HTTPS release URLs with verified `sha256` (`bundle audit check --update` when using Bundler) |

## Style

- Begin standalone Ruby files with `# frozen_string_literal: true` (Homebrew formulae follow `brew style` conventions).
- Use guard clauses for early returns to keep the happy path unindented.
- Prefer explicit keyword arguments and symbols over positional boolean flags.
- Prefer `Hash#fetch` over `[]` when a missing key represents a bug or invalid input.

## Formula & Code Design

- Pin every `url` and `resource` block to an immutable HTTPS release tag or archive with a verified `sha256` checksum—never point a stable formula to a floating branch.
- Keep `install` blocks deterministic and offline-safe: install binaries via `system "cargo", "install", *std_cargo_args` (or prebuilt release artifacts) without unpinned network downloads during installation.
- Declare `license`, `homepage`, and `desc` accurately and keep `desc` concise without repeating the formula name.
- Write a real `test do` block that exercises the installed binary's behavior (for example, invoking an MCP handshake or CLI check) rather than only checking `--version` or `--help`.

## Performance

Avoid spawning subprocesses inside loops; prefer built-in Ruby file and enumerable methods. Keep `brew audit` and `brew test` blocks fast and self-contained.

## Linter Configuration

Enforce style and safety through Homebrew's built-in RuboCop ruleset (`brew style`) for tap files, or a `.rubocop.yml` for standalone Ruby code rather than repeating rules in prose:

```yaml
AllCops:
  NewCops: enable

Layout/LineLength:
  Max: 118

Metrics/CyclomaticComplexity:
  Max: 8

Metrics/MethodLength:
  Max: 25

Security/Eval:
  Enabled: true

Security/Open:
  Enabled: true

Style/FrozenStringLiteralComment:
  Enabled: true
```

Fix every RuboCop offense. If an exception is genuinely unavoidable, disable only the single cop on that line with an inline justification comment.

## Tests

Validate every formula change locally before opening a pull request:

```bash
brew style Formula/<name>.rb
brew audit --strict Formula/<name>.rb
brew install --build-from-source Formula/<name>.rb
brew test <name>
```

For non-trivial standalone Ruby libraries, write unit tests (`rspec` or `minitest`) covering edge cases and failure paths, and use property-based testing (`rantly`) for parsers and serializers.
