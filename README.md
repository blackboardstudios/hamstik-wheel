<p align="center">
  <img
    src="assets/readme-header-logo.png"
    alt="Hamstik Wheel — autonomous work-item delivery for Hamstik"
    width="100%"
  />
</p>

Hamstik Wheel is a small, local autonomous orchestration tool for
[Hamstik](https://hamstik.com)-managed software projects. It turns a Hamstik
backlog into reviewed, committed code — one Work Item at a time, inside your
own repository, using your own validation tooling.

Wheel uses the existing [Hamstik CLI](https://github.com/blackboardstudios/hamstik-cli)
as its exclusive Hamstik integration boundary. For each eligible Work Item it
runs a Pi implementation session, validates the repository, runs a fresh,
independent Pi review/remediation session, validates again, commits the
result, asks Hamstik CLI to close the Work Item, and then repeats with the
next eligible item.

> **Status:** Early development. Hamstik Wheel is intentionally focused on a
> simple, observable, one-Work-Item-at-a-time autonomous delivery loop.

[![CI](https://github.com/blackboardstudios/hamstik-wheel/actions/workflows/ci.yml/badge.svg)](https://github.com/blackboardstudios/hamstik-wheel/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

## Quick start

### Requirements

- Git
- The [Pi coding agent](https://github.com/earendil-works/pi) (`pi` on `PATH`)
- [Hamstik CLI](https://github.com/blackboardstudios/hamstik-cli) (`hamstik` on
  `PATH`, or configured through `[hamstik].cli_path`)
- A Rust toolchain when building from source (the repository pins `stable`
  through `rust-toolchain.toml`)
- A Git repository — Wheel always operates on the repository it is started in
- An authenticated Hamstik CLI with the desired Organization/Project context
  selected

Wheel does not manage Hamstik authentication. Sign in and select context with
Hamstik CLI (`hamstik auth login`, `hamstik context init --org acme --project
HAM`); Wheel inherits exactly what Hamstik CLI resolves at runtime.

### Build from source

Wheel is not yet published to crates.io or as release archives. Build it from
source:

```bash
git clone https://github.com/blackboardstudios/hamstik-wheel.git
cd hamstik-wheel
cargo build --release
./target/release/hamstik-wheel --version
```

Optionally install the binary into Cargo's binary directory:

```bash
cargo install --path . --locked
```

### Initialize a repository

From a repository already connected to the desired Hamstik Organization/Project:

```bash
hamstik-wheel init
$EDITOR .hamstik-wheel.toml
```

`init` writes the default configuration. Review the Work Item filters
(`[hamstik]`), the model pair (`[models]`), and — importantly — set
`[validation].commands` to the checks your repository defines. The checked-in
[.hamstik-wheel.toml.example](.hamstik-wheel.toml.example) shows an annotated
configuration.

### Run one Work Item

```bash
hamstik-wheel doctor
hamstik-wheel once
```

`doctor` verifies the whole dependency chain — Git, Pi, Hamstik CLI, the
required Hamstik command surface, both configured models, and repository
readiness — before any agent runs. `once` is the recommended way to establish
trust: it selects the single highest-priority eligible Work Item, implements
it, reviews it, commits, and closes it.

### Run the wheel

```bash
hamstik-wheel run --max-items 5
```

`run` repeats the same loop until the item limit is reached or no eligible
Work Items remain. Omitting `--max-items` uses `[loop].max_items` from the
configuration (10 by default).

## Why Hamstik Wheel?

![Why Hamstik Wheel?](assets/why-hamstik-wheel.png)

- **Hamstik-native** — Hamstik CLI owns authentication, profile and context
  resolution, API compatibility, Work Item access, comments, and lifecycle
  transitions. Wheel holds no credentials and speaks no Hamstik HTTP.
- **Two-model loop** — implementation and review run in separate fresh Pi
  processes, each with its own configured model.
- **Hard completion gates** — an agent saying "done" is not sufficient.
  Repository validation and independent review must both pass before Wheel
  commits and closes the Work Item.
- **One Work Item at a time** — no fleets, worker pools, distributed
  schedulers, or worktree concurrency. The design is deliberately narrow.
- **Crash-safe and observable** — orchestration state lives under Git
  metadata, and interrupted work resumes instead of silently starting
  something new.

## How it works

![Hamstik Wheel workflow](assets/how-the-wheel-turns.png)

```text
Hamstik Work Item
        ↓
    Hamstik CLI
        ↓
 Pi implementation session
        ↓
 pre-review inspection
        ↓
 fresh Pi review/remediation session
        ↓
  repository validation
        ↓
      git commit
        ↓
Hamstik Work Item → Done
        ↓
 next eligible Work Item
```

**Wheel — not the agent — owns lifecycle truth.** Implementation and reviewer
agents never decide that a Work Item is Done, and both are explicitly
instructed not to change Work Item status, add Hamstik comments, or create Git
commits. Wheel advances the loop between phases, persists a phase marker after
every step, and calls Hamstik CLI to start or close the Work Item only after
the required gates succeed.

## Current capabilities

Today Hamstik Wheel provides:

- a project-local `.hamstik-wheel.toml` created by `init`;
- deterministic, LLM-free Work Item selection with configured status, type,
  and label filters;
- Hamstik integration exclusively through machine-readable Hamstik CLI
  surfaces, verified at runtime against the installed CLI's
  `hamstik commands --json` manifest;
- the `hamstik work context <KEY> --json` bundle as the authoritative agent
  input;
- separate implementation and review model configuration;
- a fresh Pi process for every implementation and review session;
- repository-defined validation commands, run before review and again after
  review passes;
- clean-working-tree protection before a new Work Item is selected
  (`require_clean_start`);
- a persisted baseline commit SHA for every active Work Item;
- Git commits owned by Wheel — agents are forbidden from committing;
- Work Item start/close transitions through Hamstik CLI;
- start and completion comments with baseline-keyed idempotency, each
  individually configurable;
- crash/resume orchestration state under Git metadata
  (`hamstik-wheel/state.json`);
- per-Work-Item agent and validation logs under Git metadata;
- live single-line agent activity rendering with elapsed time (TTY only);
- timestamped console output (`--timestamps local|utc|none`, local by default)
  with an optional `--log-file <PATH>` mirror of the whole run;
- `--max-items`, `max_review_cycles`, and the unattended-resilience options
  (`on_failure`, `agent_timeout_minutes`, `implement_retry`);
- `init`, `doctor`, `once`, `run`, `resume`, and `status`;
- `--timestamps` and `--log-file` output logging options.

Release packaging, a `docs/` tree, and multi-agent scale-out are future work;
concurrency beyond one Work Item is a non-goal by design.

## Commands

| Command | Purpose |
| --- | --- |
| `hamstik-wheel init` | Create `.hamstik-wheel.toml` in the current Git repository |
| `hamstik-wheel doctor` | Verify Git, Pi, Hamstik CLI, its command surface, configured models, and repository readiness; advise on stranded in_progress items and stale WIP branches |
| `hamstik-wheel once` | Process exactly one Work Item, resuming an active item first when needed |
| `hamstik-wheel run --max-items N` | Process Work Items sequentially until the requested/configured limit or no work remains |
| `hamstik-wheel resume` | Resume the interrupted active Work Item; errors when nothing is active |
| `hamstik-wheel status` | Show persisted loop state: phase, Work Item, baseline SHA, review cycle, last error, skip ledger |

`run` and `once` automatically resume an active Work Item before selecting a
new one; `resume` exists for when that should be the only thing that happens.
`--max-items` is optional and defaults to `[loop].max_items`.

## Output logging

All Wheel-generated progress lines (`Selected …`, `[claim]`, `[implement]`,
`[inspect]`, `[remediate]`, `[review]`, `[validate]`, `[commit]`, `[complete]`,
doctor results, and errors)
can be timestamped and mirrored to a file. These are global options, accepted
before the subcommand:

```bash
hamstik-wheel --timestamps utc run --max-items 5
hamstik-wheel --log-file wheel-run.log once
hamstik-wheel --verbose run --max-items 5
hamstik-wheel --no-timestamps status
```

- `--timestamps local|utc|none` — prefix every Wheel line with a millisecond
  RFC 3339 timestamp. Defaults to `local`; `--no-timestamps` is shorthand for
  `--timestamps none`.
- `--log-file <PATH>` — additionally append everything Wheel prints (with the
  selected timestamps applied) to the given file. The file is opened in append
  mode and created (including parent directories) if missing, so repeated runs
  accumulate one continuous history.
- `--verbose` — stream validation subprocess output verbatim. By default Wheel
  shows concise inspection/remediation progress and keeps the full output in
  the per-Work-Item validation log.

Timestamps apply only to lines Wheel generates itself. With `--verbose`,
validation subprocess output is streamed verbatim and mirrored into
`--log-file` when enabled. With or without `--verbose`, the complete output is
captured in per-Work-Item logs under Git metadata.

## Live agent activity

While an implementation or review session runs, Wheel renders a single
in-place console line showing what the agent is doing and for how long:

```text
[implement] CLI-52 with step-3.7-flash
[pi] ⠸ bash: cargo test --workspace · 0:03
[pi] ✓ finished · 4:12
```

- The line updates in place as the agent works — each `thinking…`,
  `calling …`, or `tool: argument` event replaces the description — and the
  elapsed clock keeps ticking between events.
- One final `[pi] ✓ finished · MM:SS` line is left in scrollback when the
  session ends, so a completed session costs exactly one line of history.
- Rendering requires an interactive terminal. With piped output (CI, logs,
  `--log-file`), the tracker is disabled entirely and nothing is rendered.
- Wheel launches Pi with `--mode json` to receive these events; `doctor` and
  `preflight` verify the installed Pi supports JSON output mode and fail with
  a clear error otherwise.
- The full event stream is captured verbatim in the per-Work-Item transcript
  logs under Git metadata (`hamstik-wheel/logs/<KEY>/…`), and the structured
  `HAMSTIK_WHEEL_RESULT` marker is extracted from the session's final
  assistant message.

## Configuration

Wheel reads `.hamstik-wheel.toml` from the repository root. It deliberately
contains no Hamstik URL, PAT, refresh token, or authentication configuration —
those belong to Hamstik CLI.

```toml
[hamstik]
# Path to the Hamstik CLI binary ("hamstik" via PATH when omitted).
cli_path = "hamstik"
# Candidate discovery is scoped to the Organization/Project selected by Hamstik CLI.
statuses = ["todo"]
item_types = ["task", "bug", "story", "feature"]
# Optional extra eligibility gate, e.g. ["agent-ready"].
label_names = []
# Assign each claimed Work Item to the authenticated Hamstik user.
assign_to_me = true

[models]
# Any model identifier Pi can resolve is valid.
implement = "step-3.7-flash"
review = "glm-5.3-flash"

[validation]
# Commands run from the repository root; all must pass before completion.
commands = [
  "./scripts/do-prechecks.py",
]

[loop]
max_items = 10
max_review_cycles = 3
on_failure = "skip"
agent_timeout_minutes = 45
implement_retry = true
provider_retries = 3
provider_retry_delay_seconds = 30

[git]
require_clean_start = true
commit = true
commit_message = "{key}: {title}"

[comments]
post_started = true
post_completed = true
```

- **`[hamstik]`** — how Wheel finds the CLI, which Work Items are eligible, and
  whether claimed Work Items are assigned to the authenticated Hamstik user.
  `assign_to_me` defaults to `true`; set it to `false` to leave assignees
  unchanged.
  Statuses are restricted to `backlog`, `todo`, `in_progress`, `in_review`,
  and `done`; types to `task`, `bug`, `story`, `feature`, and `epic`. Epics
  are excluded by default because Wheel should execute actionable child work.
- **`[models]`** — the two Pi models. Any model identifier Pi can resolve may
  be configured; `step-3.7-flash` and `glm-5.3-flash` are the defaults from
  the original local workflow, not requirements imposed by Hamstik.
- **`[validation]`** — shell commands run from the repository root (`sh -lc`
  on Unix, `cmd /C` on Windows); every command must exit 0. With no commands
  configured, `doctor` warns that the final gate relies on review only.
  Optional `[[validation.rules]]` add commands when changed paths match any
  configured literal repository-relative prefix; rules are evaluated before
  inspection and again before final validation, including new and deleted files.
- **`[loop]`** — `max_items` bounds a `run` invocation when `--max-items` is
  omitted; `max_review_cycles` bounds review/remediation iterations per Work
  Item; `on_failure = "skip"` makes the run record the failure, restore the
  item's baseline tree, and continue with the next item instead of stopping
  (`"halt"` stops the run — the default); `agent_timeout_minutes` caps each
  agent session's wall-clock time (0 disables); `implement_retry` retries the
  implementation session once when it fails to produce a parsable result
  marker. Provider errors use `provider_retries` (default 3 additional
  sessions, maximum 10) and `provider_retry_delay_seconds` (default 30).
  The delay doubles between retries, capped at 300 seconds. Set retries to
  0 to stop immediately on a provider failure.
- **`[git]`** — `require_clean_start` blocks new selection on a dirty working
  tree; `commit` controls whether Wheel commits; `commit_message` supports the
  `{key}` and `{title}` placeholders.
- **`[comments]`** — post a comment when Wheel claims a Work Item and when it
  completes it.

### Required checks for particular changes

A passing generic precheck does not establish that database upgrades or other
specialized behavior were tested. Configure those checks explicitly. For a
repository with Drizzle migrations, for example (adapt commands to the
repository and provide its test database):

```toml
[[validation.rules]]
path_prefixes = ["drizzle/", "src/db/"]
commands = ["pnpm migrations:test", "pnpm test:pg:ephemeral"]
```

Prefixes are literal, not globs: `drizzle/` matches that directory, and
`src/db/schema.ts` matches paths beginning with that string. Commands are
added to the base list once and must exit successfully at the final gate.
Changes made during review activate rules too.

Agents receive the validation configuration and must identify required
checks that it omits. The result protocol includes `unverified_checks`;
nonempty entries block completion even if the marker says `ready_for_review`
or `pass`. An unavailable required check must be reported as blocked unless
it is explicitly scheduled in Wheel's validation configuration. Wheel does
not infer required checks by parsing repository prose or agent summaries;
configure critical checks as commands/rules for a deterministic gate.

## Work Item selection

Wheel intentionally does not use an LLM to decide what should be worked on
next. Before each new selection, it discovers sprints with
`hamstik sprint list --all` in the repository's Hamstik CLI Project context.
Unarchived sprints whose server-reported state is `active` take precedence.
Wheel queries their items with `hamstik work list --sprint <ID>`, retaining
the configured status, type, and label filters and the existing skip/cooldown
rules. If multiple sprints are active, their eligible items form one pool.

If no sprint is active, or no eligible items remain in the active sprints,
Wheel falls back to the existing project-wide query. A sprint discovery or
item-query error stops selection; it is not treated as an empty sprint.
An already active Wheel item continues from its checkpoint even if sprint
membership changes.

Within either pool, candidates are ordered deterministically:

1. `in_progress` before backlog/todo when present in the candidate set;
2. priority from highest to lowest (`urgent`/`critical`/`highest`, `high`,
   `medium`/`normal`, `low`, `none`);
3. oldest creation time first when priority is equal;
4. Work Item key as a final stable tie-breaker.

A repository can make eligibility stricter with the configured filters, for
example `label_names = ["agent-ready"]`. That label convention is an optional
repository-side safeguard; Hamstik does not require it.

## Implementation and review model

```text
Work Item
   │
   ├── fresh Pi implementer
   │      model = [models].implement
   │
   └── fresh Pi reviewer/remediator
          model = [models].review
```

The implementation agent receives the authoritative
`hamstik work context <KEY> --json` bundle and the baseline Git SHA. It is
instructed to inspect the repository first, implement the Work Item
completely, add or update tests, keep changes scoped to the Work Item, and
continue any partial working-tree changes on a resumed run. It ends with a
structured `HAMSTIK_WHEEL_RESULT` marker reporting `ready_for_review` or
`blocked`.

The reviewer runs as a **fresh Pi process** with no shared conversation. The
reviewer independently inspects the Work Item, the Git diff against the
baseline, the repository architecture, tests, and acceptance criteria, plus
the pre-review validation evidence — rather than inheriting the implementer's
assumptions. It is authorized to edit the working tree to fix findings, and it
also ends with a structured marker: `pass` with zero findings, or `blocked`.

Wheel parses the structured result marker and terminal provider-error events
from each session's event stream. Provider failures retain the configured
model, resolved model, provider, and error details; they are not mislabeled
as missing result markers. Model reasoning is not interpreted.

## Completion gates

Successful completion is defined by the gates Wheel controls, not by an
agent's self-report:

```text
implementation complete
       ↓
pre-review inspection records evidence
       ↓
independent review/remediation returns PASS (zero findings)
       ↓
remediation, if the reviewer reported findings
       ↓
final validation passes
       ↓
git commit succeeds
       ↓
Hamstik close succeeds
```

All configured validation commands run before review. A nonzero pre-review
result is diagnostic evidence: Wheel reports that remediation is starting and
gives the reviewer an opportunity to repair the problem. A reviewer `pass`
that still reports findings is fed into the next remediation cycle, as is a
nonzero final validation result. Every command must exit 0 at the final gate.
Review/remediation repeats up to `max_review_cycles`; only an exhausted or
blocked loop is reported as failed. Wheel then leaves the Work Item open and
keeps its active state for `resume` in halt mode, or preserves WIP and skips
the item in skip mode.

### Unattended runs

`[loop].on_failure = "skip"` records item-specific failures (process/protocol
errors, timeouts, blocked results, or exhausted review), preserves changes on
a `wheel/wip/<KEY>` branch, restores the baseline, and attempts to return the
item to `todo`. That key is excluded for the rest of the run, so Wheel can
select the next candidate. `max_items` counts completed plus skipped items.

Across invocations, skipped items have a 30-minute cooldown. Changing the
model named in a failure bypasses its cooldown; failures without a known
model also cool down. Cooling items are filtered **before** selection, so
they do not hide other candidates. When all candidates are excluded or
cooling, the invocation finishes; it does not wait for cooldown expiry.

Provider outages are handled separately because every item using that model
may be affected. Rate limits and transient provider errors receive bounded
exponential retries. Authentication and other non-transient provider errors
stop immediately. After provider retries are exhausted, Wheel stops with a
nonzero exit **even with `on_failure = "skip"`**, retaining the active phase
and working tree. Restore provider access or change the configured model,
then run `hamstik-wheel resume`: a failed reviewer resumes review without
repeating implementation or consuming another review cycle.

Implementation process/protocol errors get one additional session when
`implement_retry = true`; review process/protocol errors always get one.
An explicit `blocked` result is not retried within the phase. Provider retry
budgets apply independently of `implement_retry`.

Wheel persists a `Skipping` phase before cleanup and recovers interrupted
cleanup on the next invocation. The skip ledger records the exact WIP branch
and commit. Future implementation prompts identify that commit, including
suffixed branches such as `wheel/wip/<KEY>-2`. For old state without WIP
metadata, Wheel discovers the highest numbered surviving branch. A skipped
item is no longer active; `resume` applies to active checkpoints, while a
later `run` or `once` can select skipped work after cooldown.

Every agent attempt and validation report gets an immutable timestamped file
under `.git/hamstik-wheel/logs/<KEY>/history/`. The usual `implement.log`,
`review-NN.log`, and validation paths remain aliases containing the latest
output. Failed final retries are retained as well as first attempts, and
older aliases are archived before their first replacement. Use
`--log-file <PATH>` to capture whole-run progress and terminal failure details.

When the gates pass, Wheel commits with `git add -A` and the configured
message template, records the commit SHA, and asks Hamstik CLI to close the
Work Item. With `commit = false`, Wheel closes the item and reports that the
changes were intentionally left uncommitted.

## Crash recovery

Wheel stores state using `git rev-parse --git-path
hamstik-wheel/state.json`, so orchestration state lives inside Git metadata
rather than the working tree: it never dirties the repository, is never
committed, and is resolved per worktree. Writes are atomic (temporary file +
rename), and agent output and validation evidence are captured under the same
metadata tree (`hamstik-wheel/logs/<KEY>/`).

If a machine or terminal dies mid-item:

```bash
hamstik-wheel status    # persisted phase, Work Item, baseline, review cycle, last error
hamstik-wheel resume    # continue the interrupted Work Item from its persisted phase
```

Resumed runs re-read the authoritative Work Item context so agent input stays
current. While an item's orchestration state is active, Wheel does not select
a new Work Item — `once` and `run` resume the active item first.

`hamstik-wheel doctor` also reports two historical failure signatures so a
crashed night's damage is visible in one command:

- **Stranded items** — Work Items assigned to you that are still `in_progress`
  but not the Wheel's active item. Selection polls only the configured
  candidate statuses, so such an item is invisible to the Wheel until it is
  moved back (`hamstik work transition <KEY> todo`).
- **Stale WIP branches** — `wheel/wip/<KEY>` branches already contained in
  `HEAD`, meaning their content was salvaged or superseded and the branch is
  a deletion candidate.

## Running unattended

tmux is deliberately optional:

```bash
tmux new -s hamstik-wheel
hamstik-wheel run --max-items 10
```

Detach with `Ctrl-b d` and reattach later with `tmux attach -t hamstik-wheel`.

tmux is only a durable terminal/session host. It is not part of Hamstik
Wheel's orchestration architecture — Wheel itself is a plain foreground
process, and its durable state lives in Git metadata.

## Relationship to Hamstik CLI

Wheel treats the installed `hamstik` binary as a local machine API
([ADR-0001](design/ADR-0001-hamstik-cli-boundary.md)) and invokes only
machine-facing commands, always with the global `--no-input --json` flags:

```text
hamstik doctor                       # dependency diagnostics before any agent runs
hamstik commands                     # manifest check for the required command surface
hamstik sprint list --all            # discover active sprints in the selected Project
hamstik work list …                  # candidate discovery with configured filters
hamstik work context <KEY>           # authoritative agent input
hamstik work start <KEY>             # claim the Work Item
hamstik work comment add <KEY>       # start/completion comments (idempotency-keyed)
hamstik work close <KEY>             # close after all gates pass
```

Before any unattended run, Wheel verifies through `hamstik commands --json`
that the installed CLI provides every required command and advertises
`--json` and `--no-input` support for it.

Wheel intentionally does not contain Hamstik HTTP/API client code, PAT/token
persistence, profile handling, URL/server configuration, API compatibility or
model refresh logic, or duplicated Hamstik workflow semantics. Those
responsibilities remain in hamstik-cli, so authentication, context resolution,
retries, and redaction behave identically for manual and automated use.

## Design documents

The detailed product and technical direction lives under
[`design/`](design/):

- [`design/PRD.md`](design/PRD.md) — product requirements
- [`design/SPEC.md`](design/SPEC.md) — technical specification
- [`design/ADR-0001-hamstik-cli-boundary.md`](design/ADR-0001-hamstik-cli-boundary.md)
  — why Hamstik CLI is the sole Hamstik integration boundary

`design/` holds internal product/architecture artifacts; user-facing
documentation belongs in this README or a future `docs/` tree.

## Contributing

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Hamstik Wheel is open-source software licensed under [Apache-2.0](LICENSE).

## Trademark

Hamstik and the Hamstik logo are trademarks of Blackboard Studios.
