---
paths:
  - "**/*.sh"
  - "**/*.bash"
---

# Bash

Start every script with `set -euo pipefail`.

Lint and check formatting before committing:

```bash
shellcheck script.sh && shfmt -i 2 -d script.sh
```

Run `shfmt -i 2 -w script.sh` to format scripts in place.

Quote every variable and argument expansion (`"$var"`, `"$@"`). Prefer `[[ ]]` over `[ ]`. Follow the repository's rule against `rm -rf`: use `trash` on macOS or move files into `.trash/` on Linux and CI. If a script must clean up a temporary directory it created itself, create it with `mktemp -d`, assert that the variable is non-empty, and scope the cleanup strictly to that variable via `trap`.
