# Hamstik Wheel

**Autonomously work through your Hamstik backlog, one item at a time.**

Hamstik Wheel is a small, local orchestration tool for software repositories managed in Hamstik. It uses the existing `hamstik` CLI as the sole integration boundary to Hamstik, runs one Pi implementation agent, runs a fresh Pi review/remediation agent with a different model, verifies repository-defined checks, commits the result, and then asks Hamstik CLI to close the Work Item.

```text
Hamstik backlog
      ↓
  Hamstik CLI
      ↓
 Hamstik Wheel
      ↓
 Step implementer
      ↓
 repository checks
      ↓
 GLM reviewer/fixer
      ↓
 repository checks
      ↓
 git commit
      ↓
 Hamstik Work Item → Done
      ↓
     repeat
```

> **Project status:** early v0.1 implementation. The architecture and command boundaries are intentionally narrow so the first usable version stays simple, observable, and reliable.

## Design principles

- **Hamstik CLI is the Hamstik boundary.** Wheel does not implement Hamstik HTTP, PAT storage, API model refresh, profiles, or context resolution.
- **One Work Item at a time.** No worker pools, worktrees, schedulers, or distributed-agent framework in v1.
- **Fresh review context.** Implementation and review are separate Pi processes and may use different models.
- **Wheel owns lifecycle truth.** Agents never mark a Work Item done. Wheel does that only after review and validation gates pass.
- **Git and Hamstik are durable state.** Wheel stores only minimal orchestration state under Git's metadata path so it never dirties the repository.
- **No tmux dependency.** Run Wheel inside tmux if you want durable terminal attachment; tmux is not part of orchestration.

## Requirements

- Git
- [Pi coding agent](https://github.com/earendil-works/pi)
- [Hamstik CLI](https://github.com/blackboardstudios/hamstik-cli)
- Rust toolchain for building from source
- An initialized Git repository
- Hamstik CLI authenticated and configured with an Organization/Project context

Hamstik CLI already provides the machine-oriented surfaces Wheel needs, including `hamstik doctor --json`, `hamstik work list --json`, `hamstik work context <KEY> --json`, `hamstik work start`, `hamstik work close`, and Work Item comments.

## Build

```bash
cargo build --release
```

The resulting binary is:

```bash
./target/release/hamstik-wheel
```

## Quick start

From a repository already connected to the desired Hamstik Organization/Project with Hamstik CLI:

```bash
hamstik-wheel init
$EDITOR .hamstik-wheel.toml
hamstik-wheel doctor
hamstik-wheel once
```

After you trust the behavior for individual Work Items:

```bash
hamstik-wheel run --max-items 5
```

For an unattended terminal session, tmux is sufficient:

```bash
tmux new -s hamstik-wheel
hamstik-wheel run --max-items 10
```

Detach with `Ctrl-b d` and later reattach with:

```bash
tmux attach -t hamstik-wheel
```

## Commands

```text
hamstik-wheel init                 Create .hamstik-wheel.toml
hamstik-wheel doctor               Verify Git, Pi, Hamstik CLI, context, and config
hamstik-wheel once                 Process one Work Item (or resume an interrupted one)
hamstik-wheel run                  Process Work Items until the configured/requested limit
hamstik-wheel resume               Resume the current interrupted Work Item
hamstik-wheel status               Show persisted loop state
```

## Configuration

Wheel reads `.hamstik-wheel.toml` from the repository root. It deliberately does **not** contain Hamstik URLs or credentials; those belong to Hamstik CLI.

```toml
[hamstik]
statuses = ["todo"]
item_types = ["task", "bug", "story", "feature"]
label_names = []

[models]
implement = "step-3.7-flash"
review = "glm-5.3-flash"

[validation]
commands = ["./scripts/do-prechecks.py"]

[loop]
max_items = 10
max_review_cycles = 3

[git]
require_clean_start = true
commit = true
commit_message = "{key}: {title}"
```

Any model identifier Pi can resolve can be used. The defaults reflect the original local workflow but are not a Hamstik product requirement.

## Selection policy

V1 intentionally avoids an AI planner. Candidate Work Items come from the current Hamstik CLI Project context and the configured status/type/label filters. Wheel then selects deterministically:

1. `in_progress` before backlog/todo when present in the candidate set;
2. priority from highest to lowest;
3. oldest creation time first when priority is equal;
4. Work Item key as a final stable tie-breaker.

A repository can make eligibility stricter with the configured Hamstik CLI filters, for example `label_names = ["agent-ready"]`.

## Agent contract

The implementation agent receives the authoritative `hamstik work context <KEY> --json` bundle and the baseline Git SHA. It is told to implement the Work Item completely, add/update tests, avoid unrelated changes, and return a structured final marker.

The reviewer runs as a **fresh Pi process**. It independently inspects the authoritative Work Item, repository, baseline diff, and pre-review validation evidence. It may modify the working tree to resolve findings. Wheel accepts completion only when the reviewer reports PASS and all configured validation commands pass.

Agents do not call `hamstik work close`; Wheel owns Work Item lifecycle transitions.

## Crash recovery

Wheel stores state using `git rev-parse --git-path hamstik-wheel/state.json`. This keeps orchestration state inside Git metadata rather than the working tree. If a machine or terminal dies mid-item:

```bash
hamstik-wheel resume
```

Wheel resumes the persisted phase instead of selecting a new Work Item.

## Design documents

Internal product/design artifacts live under [`design/`](design/):

- [`PRD.md`](design/PRD.md)
- [`SPEC.md`](design/SPEC.md)
- [`ADR-0001-hamstik-cli-boundary.md`](design/ADR-0001-hamstik-cli-boundary.md)

User-facing documentation should remain outside `design/` as the project grows.

## License

Apache License 2.0. See [`LICENSE`](LICENSE).
