# Hamstik Wheel — Technical Specification

**Status:** Draft / initial implementation contract
**Target:** v0.1

## 1. Architecture

Hamstik Wheel is a synchronous Rust CLI. It coordinates three existing local tools:

```text
                      ┌──────────────┐
                      │   Hamstik    │
                      └──────▲───────┘
                             │ Public API v1
                      ┌──────┴───────┐
                      │ hamstik CLI  │
                      └──────▲───────┘
                             │ JSON + exit codes
┌──────────────┐      ┌──────┴───────┐       ┌──────────────┐
│     Git      │◄────►│ Hamstik Wheel│──────►│      Pi      │
└──────────────┘      └──────┬───────┘       └──────────────┘
                             │
                             ▼
                    repository validation
```

Wheel contains no Hamstik HTTP client and no model-provider client.

## 2. Module layout

```text
src/
├── main.rs        CLI parsing and top-level dispatch
├── config.rs      .hamstik-wheel.toml loading/defaults/init
├── hamstik.rs     typed subprocess boundary around hamstik CLI
├── pi.rs          Pi subprocess runner, prompts, result parsing
├── git.rs         repository/root/head/diff/commit/git-path helpers
├── validation.rs  repository command execution and reports
├── logging.rs     timestamped console logger with optional file mirror
├── state.rs       persisted state schema/read/write/clear
└── loop_engine.rs state machine and phase orchestration
```

Dependencies should remain intentionally small.

## 3. Configuration

### 3.1 File

`<repo-root>/.hamstik-wheel.toml`

Example:

```toml
[hamstik]
statuses = ["todo"]
item_types = ["task", "bug", "story", "feature"]
label_names = []
assign_to_me = true

[models]
implement = "step-3.7-flash"
review = "glm-5.3-flash"

[validation]
commands = ["./scripts/do-prechecks.py"]

[loop]
max_items = 10
max_review_cycles = 3
on_failure = "halt"
agent_timeout_minutes = 45
implement_retry = true

[git]
require_clean_start = true
commit = true
commit_message = "{key}: {title}"

[comments]
post_started = true
post_completed = true
```

### 3.2 Resolution

- Wheel resolves repository root via `git rev-parse --show-toplevel`.
- Configuration is always read relative to that root.
- Wheel does not inspect `.hamstik.toml`; Hamstik CLI owns Hamstik context resolution.
- Missing Wheel config is an error except for `init`.

## 4. Hamstik CLI integration

### 4.1 Process rules

Every machine invocation MUST:

- execute from the repository root;
- use `--no-input`;
- request `--json` where supported;
- capture stdout for machine parsing;
- preserve/surface stderr on failure;
- treat any non-zero process status as an integration failure.

### 4.2 Commands

#### Readiness

```bash
hamstik --version
hamstik --no-input --json doctor
```

#### Candidate discovery

```bash
hamstik --no-input --json work list --status todo --type task --type bug --type story --type feature --all
```

#### Authoritative context bundle

```bash
hamstik --no-input --json work context HAM-123 --comments 100 --activity 25
```

The entire returned JSON object is preserved and passed to agents. Wheel should avoid reconstructing the authoritative Work Item from multiple endpoints.

#### Start

```bash
hamstik --no-input --json work start HAM-123
```

When assignment is enabled, Wheel then assigns the Work Item to the authenticated
Hamstik user:

```bash
hamstik --no-input --json work edit HAM-123 --assignee me
```

#### Add comment

Body is sent over stdin to avoid fragile shell quoting:

```bash
printf '%s' "$BODY" | hamstik --no-input --json work comment add HAM-123 --body-file -
```

#### Complete

```bash
hamstik --no-input --json work close HAM-123
```

### 4.3 Discovery JSON tolerance

The initial code should deserialize discovery as `serde_json::Value` and accept common envelope shapes without coupling to the full generated Hamstik API model:

- top-level array;
- `{ "items": [...] }`;
- `{ "workItems": [...] }`;
- `{ "data": [...] }`;
- `{ "data": { "items": [...] } }`.

Each candidate needs only:

- `key` (required);
- `title` (required for display/commit message, default key if absent);
- `status` (string or object name/key);
- `priority` (string or object name/key);
- `createdAt` / `created_at` (optional stable secondary sort input).

All implementation requirements come from `work context`, not the list summary.

## 5. Selection algorithm

Input is the candidate list returned from the selected Hamstik CLI Project using the configured status/type/label filters.

Normalize status case and punctuation. Rank:

```text
in_progress / in progress  -> 0
other                       -> 1
```

Normalize priority:

```text
urgent / critical / highest -> 0
high                        -> 1
medium / normal             -> 2
low                         -> 3
none / no_priority          -> 4
unknown                     -> 5
```

Sort tuple:

```text
(status_rank, priority_rank, created_at, key)
```

This is intentionally deterministic. The Hamstik CLI filters remain the primary eligibility policy.

## 6. Git integration

### 6.1 Repository root

```bash
git rev-parse --show-toplevel
```

### 6.2 Baseline

```bash
git rev-parse HEAD
```

Baseline is captured before implementation and persisted in state.

### 6.3 Clean start

When `require_clean_start = true` and there is no active Wheel state:

```bash
git status --porcelain
```

must be empty.

When an active Wheel state exists, a dirty tree is expected and must not by itself block resume.

### 6.4 Diff

Review prompt tells Pi to inspect the repository directly. Wheel may also collect:

```bash
git diff <baseline> -- .
git diff --cached <baseline> -- .
```

for logging/evidence.

### 6.5 Commit

When enabled:

```bash
git add -A
git commit -m '<expanded template>'
git rev-parse HEAD
```

No push occurs in v0.1.

## 7. Persistent state

### 7.1 Location

Resolve with:

```bash
git rev-parse --git-path hamstik-wheel/state.json
```

This also behaves correctly for linked worktrees, where `.git` may be a file rather than a directory.

Logs use sibling path:

```text
<git-path>/hamstik-wheel/logs/
```

### 7.2 Schema

```json
{
  "schemaVersion": 1,
  "phase": "reviewing",
  "current": {
    "key": "HAM-123",
    "title": "Example",
    "baselineSha": "abc123...",
    "commitSha": null
  },
  "reviewCycle": 1,
  "completedThisRun": 0,
  "lastError": null,
  "updatedAt": "2026-09-17T21:30:00Z"
}
```

### 7.3 Phases

Rust enum serialized in snake_case:

- `idle`
- `selected`
- `claimed`
- `implementing`
- `pre_review_validation`
- `reviewing`
- `final_validation`
- `committing`
- `closing`

### 7.4 Atomic write

State writes SHOULD use write-temp + rename when practical. A partially written state file must not be silently treated as no state.

## 8. Pi integration

### 8.1 Invocation

Pi supports non-interactive print mode, model selection, and an NDJSON event
stream. Wheel invokes:

```bash
pi --model '<MODEL>' --no-session --mode json -p 'Execute the Hamstik Wheel task supplied on stdin.'
```

The complete prompt is piped to stdin. `--no-session` deliberately prevents conversational state from carrying between roles or Work Items.

Wheel reads the NDJSON event stream line-by-line and:

- renders a single in-place activity line (`[pi] <spinner> <description> · <elapsed>`) when stdout is an interactive terminal, leaving one final `[pi] ✓ finished · MM:SS` line in scrollback; nothing is rendered when stdout is not a TTY or `--log-file` is set;
- retains the complete event stream verbatim as the transcript for result parsing and logging.

Preflight and doctor verify the installed Pi supports `--mode json` and fail with a clear error otherwise.

### 8.2 Structured terminal marker

Agents MUST end with exactly one machine marker line:

```text
HAMSTIK_WHEEL_RESULT={"status":"ready_for_review","summary":"...","findings":[]}
```

Reviewer PASS:

```text
HAMSTIK_WHEEL_RESULT={"status":"pass","summary":"...","findings":[]}
```

Reviewer blocked/non-pass:

```text
HAMSTIK_WHEEL_RESULT={"status":"blocked","summary":"...","findings":["..."]}
```

Wheel searches output from the end for the marker, parses the suffix as JSON, and rejects a missing/malformed marker.

Because the transcript is the NDJSON event stream, the marker arrives
JSON-escaped inside the final assistant message. Extraction handles:

- a plain (unescaped) marker line as before;
- a marker inside any string value of a parsed event line: the line is
  decoded with serde_json and every decoded string value is searched for the
  marker prefix, then the balanced `{...}` payload is extracted (with JSON
  string-state-aware brace counting) and used directly;
- a fallback brace-count heuristic on the raw bytes for lines that are not
  valid JSON.

### 8.3 Implementation prompt invariants

The implementation prompt MUST say, in substance:

- authoritative requirements are in the supplied Hamstik context;
- satisfy every acceptance criterion;
- inspect existing code/conventions first;
- add/update tests;
- run useful local tests during implementation;
- do not change unrelated behavior merely to simplify the task;
- do not change Hamstik lifecycle/status/comments;
- do not create a Git commit;
- if materially blocked/ambiguous, stop safely and report `blocked`;
- otherwise finish with `ready_for_review` marker.

### 8.4 Review prompt invariants

The review prompt MUST say, in substance:

- this is an independent review; do not trust prior conclusions;
- compare actual working tree to authoritative context;
- inspect diff from baseline and surrounding architecture;
- evaluate all requirements/AC;
- evaluate correctness/regressions/error handling/security/authz/data isolation/tests/complexity/docs;
- consider supplied validation failures explicitly;
- fix every actionable finding in the working tree;
- re-run appropriate tests and re-review after fixes;
- do not close/comment/commit;
- return `pass` only with zero unresolved actionable findings;
- return `blocked` when safe completion is impossible.

## 9. Validation

Each configured command executes sequentially from repository root.

Result model:

```rust
struct ValidationCommandResult {
    command: String,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

struct ValidationReport {
    passed: bool,
    commands: Vec<ValidationCommandResult>,
}
```

All commands must pass for final completion.

Pre-review failures are included verbatim (bounded if necessary in future) in the reviewer prompt.

## 10. Loop algorithm

Pseudo-code:

```text
run(limit):
  doctor-lightweight-preconditions
  completed = 0

  while completed < limit:
    if active state exists:
      resume current item
    else:
      require clean tree (if configured)
      candidates = hamstik.work_list(config.statuses, config.item_types, config.label_names)
      if candidates empty: return success
      item = deterministic_select(candidates)
      baseline = git.head()
      persist SELECTED(item, baseline)

    context = hamstik.context(item.key)

    if phase <= SELECTED:
      hamstik.start(item.key) unless already in-progress
      optional started comment
      persist CLAIMED

    if phase <= CLAIMED/IMPLEMENTING:
      persist IMPLEMENTING
      output = pi.run(implement_model, implementation_prompt(context, baseline))
      result = parse_marker(output)
      if result.status != ready_for_review: stop safely
      persist PRE_REVIEW_VALIDATION

    pre_report = validation.run_all()

    for cycle in current_cycle..max_review_cycles:
      persist REVIEWING(cycle)
      output = pi.run(review_model, review_prompt(context, baseline, pre_report))
      review = parse_marker(output)
      if review.status == blocked: stop safely

      persist FINAL_VALIDATION
      final_report = validation.run_all()

      if review.status == pass && review.findings.empty && final_report.passed:
         break success

      pre_report = final_report

    if no successful review cycle: stop safely

    if commit enabled:
      persist COMMITTING
      sha = git.commit(...)
    else:
      sha = null  # completion is explicitly recorded as uncommitted

    persist CLOSING
    optional completion comment
    hamstik.close(item.key)

    clear current state
    completed += 1
```

## 11. Resume semantics

Resume uses persisted phase conservatively:

- `selected`: re-read context and continue claim;
- `claimed`/`implementing`: rerun a fresh implementer. Prompt explicitly tells it to inspect and continue any partial working-tree changes rather than assume a pristine tree;
- `pre_review_validation`: rerun validation;
- `reviewing`: launch a fresh reviewer for that cycle;
- `final_validation`: rerun final validation and, if not sufficient, another reviewer cycle if allowed;
- `committing`: inspect whether a new commit beyond baseline already exists before blindly creating another commit;
- `closing`: retry comment/close idempotently where safe.

The initial implementation may conservatively stop for ambiguous commit/close recovery rather than guess.

## 12. Doctor behavior

Doctor output is human-readable and returns non-zero if any required check fails.

Checks:

1. `git --version`;
2. repository root resolution;
3. Wheel config parse;
4. `pi --version`;
5. optional `pi --list-models <pattern>` sanity for both configured models;
6. `pi --mode json` support (required for activity streaming and marker extraction);
7. `hamstik --version`;
8. `hamstik --no-input --json doctor` exit success;
9. no incompatible active state;
10. clean working tree when required and no active state;
11. non-empty validation command set (warning rather than fatal may be configurable later).

## 13. Logging

### 13.1 Captured agent/validation output

V0.1 SHOULD store captured agent/validation outputs under Git metadata:

```text
hamstik-wheel/logs/<KEY>/implement.log
hamstik-wheel/logs/<KEY>/pre-review-validation.log
hamstik-wheel/logs/<KEY>/review-01.log
hamstik-wheel/logs/<KEY>/final-validation-01.log
```

Logs must not intentionally include credentials. Hamstik CLI output is already expected to be redacted; Wheel should never add environment dumps.

### 13.2 Console logging options

Wheel-generated progress lines (selection, phase markers, doctor results, and
errors) pass through a shared logger that:

- optionally prefixes each line with a millisecond RFC 3339 timestamp
  (`--timestamps local|utc|none`; `local` is the default and `--no-timestamps`
  is shorthand for `--timestamps none`);
- optionally mirrors every printed line, with timestamps applied, into an
  append-mode file (`--log-file <PATH>`), creating the file and its parent
  directories when missing.

Agent and validation subprocess output streams through verbatim (no timestamp
prefix) into both the terminal and the `--log-file` mirror when enabled, and is
captured verbatim into the per-Work-Item logs above.

### 13.3 Live agent activity rendering

While a Pi session runs, Wheel renders one in-place line (carriage-return
redraw, space-padded erase) describing current agent activity:

- description comes from the event stream: `thinking…` on thinking start,
  `calling <tool>…` on toolcall start, `<tool>: <summary>` on tool execution
  start (summary is the first of `command`, `path`, `file_path`, `pattern`,
  `query`, `url`, `description` arguments, whitespace-collapsed and truncated
  to ~48 characters);
- a spinner and a monotonically increasing `M:SS` / `H:MM:SS` elapsed clock;
- on session end the tracker prints one final `[pi] ✓ finished · MM:SS` line,
  so a session contributes exactly one line to scrollback;
- rendering is disabled entirely when stdout is not a Linux terminal or when
  `--log-file` is set.

## 14. Completion comment format

Suggested Markdown:

```markdown
Hamstik Wheel completed automated implementation.

- Commit: `abc1234`
- Implementation model: `step-3.7-flash`
- Review model: `glm-5.3-flash`
- Review cycles: 1
- Independent review: PASS
- Final repository validation: PASS
- Outstanding actionable findings: 0
```

Start comment should be similarly concise.

## 15. Error handling

By default, errors are fatal to the active loop (`[loop].on_failure = "halt"`).
With `on_failure = "skip"`, a failed Work Item is handled as follows and the
run continues with the next item:

1. the failure is recorded in persisted state (`lastError`) and in a failure
   comment on the Work Item (idempotency-keyed, best-effort);
2. in-progress working-tree changes, when any exist, are committed to a
   `wheel/wip/<KEY>` branch (never touching the current branch's history);
3. the working tree is restored to that item's recorded baseline
   (`git reset --hard <baseline>` and `git clean -fd`) so the next
   selection's clean-tree check passes; a failed restore or preservation
   halts the run regardless of mode;
4. the item is transitioned back to `todo` (best-effort) so it stays in the
   eligible pool for later runs;
5. active state is cleared and the loop advances.

Additional resilience controls:

- `[loop].agent_timeout_minutes` caps each agent session's wall-clock time;
  on expiry the Pi process is killed, the partial event transcript is kept,
  and the session is treated as a failure (retried once for implementation
  when `implement_retry` is enabled). `0` disables the cap.
- `[loop].implement_retry = true` re-runs the implementation session exactly
  once when the first attempt fails to produce a parsable result marker;
  explicit `blocked` results and non-marker-producing retries are final.
- `run` counts skipped items and reports them in the run summary; `once`
  treats a skipped item as a completed outcome.

Wheel MUST NOT:

- close a Work Item after a failed gate;
- erase working-tree changes on failure without first preserving them on a
  `wheel/wip/<KEY>` branch (the skip path preserves before restoring);
- automatically stash unrelated user changes;
- select a new Work Item while active state exists;
- retry authentication errors indefinitely;
- retry a `blocked` implementation/review result;
- parse human-formatted Hamstik output when JSON was expected.

## 16. Testing strategy

Unit tests should cover:

- config defaults/deserialization, including `on_failure`, timeout, and retry options;
- candidate envelope parsing;
- candidate status/priority ordering;
- Pi result marker parsing, including markers embedded in NDJSON event lines;
- event-to-activity-description mapping and elapsed-time formatting;
- state serialization;
- commit-message expansion;
- validation report pass/fail aggregation.

Integration tests should later use fake executable scripts placed earlier in `PATH` for `hamstik`, `pi`, and optionally `git`, allowing the full loop to be tested without network/model calls.

## 17. Release scope boundary

V0.1 is successful when `once` is safe and dependable. `run` is simply repeated `once` behavior with bounded count; it must not introduce concurrency or separate orchestration concepts.
