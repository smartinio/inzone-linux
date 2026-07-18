# Contributing

Contributions and hardware reports are welcome.

Before submitting a change:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo +1.97.1 llvm-cov --all-features --all-targets --workspace --locked \
  --no-default-ignore-filename-regex \
  --fail-uncovered-lines 0 \
  --fail-uncovered-functions 0 \
  --show-missing-lines
```

Coverage requires Rust 1.97.1 with `llvm-tools-preview` and `cargo-llvm-cov`
0.8.6. CI rejects even one uncovered code-generated source line or function,
including integration-test source, and disables cargo-llvm-cov's default
filename exclusions. The zero-uncovered gates are intentional: LLVM's
percentage summary counts partially exercised duplicate Rust test/production
instantiations, while these gates evaluate whether each source line and
function was exercised by at least one test.

Keep captured reports free of serial numbers or other unique identifiers.
Protocol-changing patches should document the device USB ID, request semantics,
and a redacted response. Firmware operations are out of scope. Setting-changing
commands require an explicit safety discussion and must not share the
battery-query code path.
