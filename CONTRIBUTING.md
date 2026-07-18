# Contributing

Contributions and hardware reports are welcome.

Before submitting a change:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
```

Keep captured reports free of serial numbers or other unique identifiers.
Protocol-changing patches should document the device USB ID, request semantics,
and a redacted response. Firmware operations are out of scope. Setting-changing
commands require an explicit safety discussion and must not share the
battery-query code path.
