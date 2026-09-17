# Contributing to Hamstik Wheel

Thanks for helping improve Hamstik Wheel.

## Development

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
