# Contributing

Contributions and hardware reports are welcome.

Before submitting a change:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo +1.97.1 llvm-cov --all-features --all-targets --workspace --locked \
  --fail-under-functions 100 \
  --fail-under-lines 100 \
  --fail-under-regions 100 \
  --fail-uncovered-lines 0 \
  --fail-uncovered-regions 0 \
  --fail-uncovered-functions 0 \
  --show-missing-lines
```

Coverage requires Rust 1.97.1 with `llvm-tools-preview` and `cargo-llvm-cov`
0.8.6. CI requires every production function, line, and LLVM region to report
100.00% coverage with zero misses. Unit and integration tests all run under
instrumentation, while cargo-llvm-cov's standard filename filter keeps test
harness and assertion internals out of the production coverage denominator.

Keep captured reports free of serial numbers or other unique identifiers.
Protocol-changing patches should document the device USB ID, request semantics,
and a redacted response. Firmware operations are out of scope. Setting-changing
commands require an explicit safety discussion and must not share the
battery-query code path.
