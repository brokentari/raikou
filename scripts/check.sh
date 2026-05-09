#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

cargo +nightly fmt --all --check
cargo clippy --all-targets --locked -- -D warnings

files=$(git ls-files --cached --others --exclude-standard '*.rs')
# shellcheck disable=SC2086
awk '
  FNR == 1 { prev = 0 }
  /\/\*/ { print FILENAME ":" FNR ": block comment"; bad = 1 }
  /^[[:space:]]*\/\// {
    if (prev) { print FILENAME ":" FNR ": two comment lines in a row"; bad = 1 }
    if (length($0) > 100) { print FILENAME ":" FNR ": comment longer than 100 columns"; bad = 1 }
    prev = 1
    next
  }
  { prev = 0 }
  END { exit bad }
' $files

cargo test --locked
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked
