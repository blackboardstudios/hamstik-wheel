# Contributing to Hamstik Wheel

Thanks for helping improve Hamstik Wheel.

## Development

Linux integration tests require Node.js 22+ at `/usr/bin/node` and Bubblewrap at
`/usr/bin/bwrap`, with unprivileged user namespaces enabled. `cargo test` runs the
real sandbox regressions via Node in addition to the Rust tests. The Pi extension
load check can also be exercised by `hamstik-wheel doctor` with an installed Pi.

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

Keep changes focused. The project's core constraint is intentional simplicity: one repository, one Work Item at a time, Hamstik CLI for Hamstik access, Pi for agent execution, Git for source state.

Internal architecture/product decisions belong under `design/`. User-facing documentation belongs in `README.md` or a future `docs/` tree.

## Pull requests

Please include:

- the problem being solved;
- behavioral changes;
- tests added/updated;
- any design/compatibility implications for Hamstik CLI or Pi.

By contributing, you agree that your contributions are licensed under Apache-2.0.
