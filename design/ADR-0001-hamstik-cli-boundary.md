# ADR-0001: Use Hamstik CLI as the sole Hamstik integration boundary

**Status:** Accepted
**Date:** 2026-09-17

## Context

Hamstik Wheel needs to discover Work Items, retrieve complete Work Item context, transition lifecycle state, and add comments. Hamstik already has an official Rust CLI designed for developers, automation, CI/CD, and coding agents.

Hamstik CLI already owns concerns that Wheel would otherwise have to duplicate:

- authentication and secure credential storage;
- profiles and Organization/Project context resolution;
- Public API v1 request construction;
- retries and idempotency behavior;
- TLS/network behavior;
- stable machine-readable JSON output and exit codes;
- OpenAPI/API compatibility management;
- command discovery/documentation;
- diagnostics through `hamstik doctor`;
- future CLI/API model refresh behavior.

Implementing Hamstik HTTP access directly inside Wheel would create a second integration stack with overlapping responsibilities, duplicated configuration, duplicated security-sensitive credential handling, and tighter coupling to Hamstik API evolution.

## Decision

Hamstik Wheel will invoke the installed `hamstik` CLI for all Hamstik operations.

Wheel will treat Hamstik CLI as a local machine API:

```text
Wheel → subprocess invocation → JSON stdout / exit code / stderr → Hamstik CLI
```

Wheel will not:

- store or request Hamstik PATs;
- implement login/profile management;
- link the generated Hamstik API client crate directly;
- call `/api/v1` endpoints itself;
- inspect Hamstik database/internal application state.

Wheel will prefer Hamstik CLI `--json` and `--no-input` surfaces and will stop on non-zero CLI status rather than attempt to repair authentication/API failures itself.

## Consequences

### Positive

- Wheel remains small and focused on orchestration.
- Authentication and secrets have a single owner.
- Hamstik API changes are usually absorbed by Hamstik CLI rather than requiring Wheel changes.
- Users get identical context/profile behavior between manual CLI usage and Wheel automation.
- Existing Hamstik CLI diagnostics and redaction behavior are reused.
- The design encourages improving machine-facing Hamstik CLI contracts for all automation users rather than adding Wheel-only APIs.

### Negative

- `hamstik` becomes a required runtime dependency.
- Wheel depends on stable command/JSON contracts from Hamstik CLI.
- Subprocess startup adds minor overhead compared with an in-process client; this is negligible relative to LLM execution time.
- Wheel must provide clear minimum-version/capability errors as the required command surface evolves.

## Compatibility policy

V0.1 documents the required Hamstik CLI commands and verifies them at runtime through `hamstik commands --json`. Wheel checks that every required command exists and advertises machine-readable JSON and non-interactive operation before an unattended run is considered healthy.

If Wheel needs a Hamstik operation that the CLI cannot expose reliably in machine-readable form, the preferred resolution is to improve Hamstik CLI first rather than bypass it from Wheel.
