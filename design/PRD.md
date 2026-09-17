# Hamstik Wheel — Product Requirements Document

**Status:** Draft for v0.1
**Product:** Hamstik Wheel
**Repository:** `blackboardstudios/hamstik-wheel`
**License:** Apache-2.0

## 1. Summary

Hamstik Wheel is a small local CLI that autonomously processes software-development Work Items from a Hamstik Project one at a time. It delegates implementation to one Pi model, delegates independent code review and remediation to a fresh Pi process using a second model, verifies repository-defined checks, commits successful changes, records a completion summary on the Work Item, marks the Work Item complete through Hamstik CLI, and repeats.

The product is intentionally narrow. It is not an agent platform, daemon, scheduler, IDE, CI system, issue tracker, or multi-worker fleet. Its purpose is to make a single trusted software-delivery loop simple enough to understand and dependable enough to leave running in a terminal.

## 2. Problem

A well-written Hamstik Work Item already contains the information a coding agent needs: title, description, acceptance criteria, discussion/context, relationships, and current workflow state. Pi can implement coding tasks effectively, and a second model can provide useful independent review. Today the human still has to manually repeat the orchestration:

1. inspect the backlog;
2. choose the next Work Item;
3. start it;
4. prompt an implementation model;
5. run checks;
6. start a separate review model;
7. apply/fix review findings;
8. re-run checks;
9. commit;
10. update and close the Work Item;
11. repeat.

This repetitive control-plane work is exactly what Hamstik Wheel should automate.

## 3. Goals

### 3.1 Primary goals

1. Process eligible Hamstik Work Items sequentially without human re-prompting between normal phases.
2. Use Hamstik CLI for all Hamstik access so Wheel inherits its configuration, authentication, API compatibility, retries, JSON output, and future model-refresh behavior.
3. Keep implementation and review independent by launching a fresh Pi process for each role.
4. Permit different Pi models for implementation and review.
5. Never mark a Work Item complete unless independent review passes and repository validation passes.
6. Persist enough local state to resume safely after interruption.
7. Keep behavior observable from a normal terminal and usable inside tmux without depending on tmux.
8. Be suitable for public use by Hamstik users without requiring the author's exact local models.

### 3.2 Secondary goals

1. Produce a useful Hamstik audit trail with start/completion comments.
2. Produce local phase logs that are easy to inspect when a run fails.
3. Make Work Item selection deterministic and explainable.
4. Refuse unsafe advancement rather than guessing through malformed tool output or failed gates.

## 4. Non-goals for v0.1

The first release will not provide:

- parallel Work Item processing;
- Git worktree orchestration;
- pull-request creation or merge automation;
- a web UI;
- a background service/daemon;
- cloud-hosted workers;
- distributed queues or message buses;
- a database;
- a generic tracker/plugin framework;
- native Hamstik HTTP/API code;
- credential storage;
- model-provider SDK integration;
- AI-based project planning or reprioritization;
- automatic resolution of materially ambiguous or conflicting requirements.

## 5. Personas

### 5.1 Primary: Hamstik developer

A developer maintains one or more repositories whose work is tracked in Hamstik. They already use Hamstik CLI and Pi and want a low-friction way to let well-specified Work Items progress autonomously while retaining strong review and validation gates.

### 5.2 Secondary: Hamstik project maintainer

A maintainer wants a repeatable local automation pattern that demonstrates the value of clear Work Item descriptions and acceptance criteria without adopting a large agent platform.

## 6. User stories

### US-1 — Initialize a repository

As a developer, I can run `hamstik-wheel init` in a Git repository and receive a small, documented configuration file without duplicating Hamstik credentials or server settings.

### US-2 — Verify readiness

As a developer, I can run `hamstik-wheel doctor` and know whether Git, Pi, Hamstik CLI, Hamstik context, configuration, models, and repository state are suitable for a run.

### US-3 — Process exactly one Work Item

As a cautious adopter, I can run `hamstik-wheel once` so Wheel selects or resumes one Work Item, completes the full implementation/review/validation/commit/close flow, and then exits.

### US-4 — Process multiple Work Items

As a developer, I can run `hamstik-wheel run --max-items N` and have Wheel repeat the same bounded process up to N successful Work Items or until no eligible work remains.

### US-5 — Independent review

As a developer, I can configure a separate reviewer model so review begins in a fresh Pi process that does not inherit the implementer's conversational context.

### US-6 — Resume after interruption

As a developer, I can run `hamstik-wheel resume` after a crash/reboot/terminal loss and Wheel continues the current Work Item rather than selecting another and mixing changes.

### US-7 — Understand failure

As a developer, when Wheel stops, I can see which phase failed, the current Work Item key, relevant command/agent output, and a safe next action.

## 7. Functional requirements

### FR-1 — Repository configuration

Wheel MUST load `.hamstik-wheel.toml` from the Git repository root.

The configuration MUST support at least:

- candidate status/type/label filters;
- implementation model;
- review model;
- validation command list;
- maximum Work Items per run;
- maximum review cycles;
- clean-start requirement;
- commit enable/disable;
- commit message template;
- start/completion comment enable/disable.

Wheel MUST NOT require Hamstik URL, PAT, profile, or credential configuration.

### FR-2 — Hamstik CLI boundary

Wheel MUST invoke the installed `hamstik` binary rather than call Hamstik HTTP endpoints directly.

V0.1 integrations SHALL use machine-readable Hamstik CLI surfaces where available, including:

- `hamstik doctor --json --no-input`;
- `hamstik work list --status todo --type task --type bug --type story --type feature --all --json --no-input`;
- `hamstik work context <KEY> --json --no-input`;
- `hamstik work start <KEY> --json --no-input`;
- `hamstik work close <KEY> --json --no-input`;
- `hamstik work comment add <KEY> --body-file - --json --no-input`.

Wheel MUST treat non-zero Hamstik CLI exit codes as failures and MUST surface stderr without attempting to reinterpret API/authentication failures itself.

### FR-3 — Work Item discovery and deterministic selection

Wheel MUST retrieve candidates using the configured project-scoped Hamstik CLI filters.

Wheel MUST select deterministically. Initial policy:

1. prefer `in_progress` candidates;
2. then sort by priority descending;
3. then creation time ascending;
4. then Work Item key ascending.

Wheel SHOULD permit projects to restrict eligibility with configured `work list` filters such as `label_names = ["agent-ready"]` rather than hard-coding an eligibility label in the binary.

### FR-4 — Claim/start

Before implementation begins, Wheel MUST retrieve authoritative Work Item context and request `hamstik work start <KEY>` when the Work Item is not already in progress.

When enabled, Wheel SHOULD add a concise comment identifying the automated run, implementation model, review model, and baseline Git SHA.

### FR-5 — Implementation agent

Wheel MUST launch Pi as a separate process using the configured implementation model.

The implementation prompt MUST include:

- the Work Item key/title;
- the complete machine-readable Hamstik context bundle;
- baseline Git SHA;
- explicit responsibility for satisfying all requirements and acceptance criteria;
- requirement to add/update tests where appropriate;
- requirement to inspect existing architecture/conventions before changing code;
- prohibition on closing/changing Hamstik Work Item lifecycle itself;
- prohibition on committing unless future configuration explicitly delegates commit ownership;
- a structured terminal result contract.

The implementer MUST operate in the target repository working tree.

### FR-6 — Pre-review validation

After implementation, Wheel MUST run all configured validation commands.

Pre-review validation failures MUST be preserved as reviewer evidence. A pre-review failure MAY proceed to review/remediation; it MUST NOT permit completion.

### FR-7 — Independent reviewer/remediator

Wheel MUST launch a fresh Pi process using the configured review model.

The review prompt MUST include:

- authoritative Work Item context;
- baseline Git SHA;
- current repository/diff instructions;
- pre-review validation report;
- explicit instruction not to trust the implementation agent's conclusions;
- review areas including requirements/AC coverage, correctness, regressions, architecture consistency, error handling, security, authorization/data isolation, tests, unnecessary complexity, and required documentation;
- permission to modify the working tree to fix findings;
- requirement to re-review after fixes;
- structured PASS/BLOCKED result contract.

### FR-8 — Final validation gate

Wheel MUST run configured validation after the reviewer finishes.

A Work Item MUST NOT proceed to commit/close unless:

- reviewer result is PASS;
- reviewer reports no unresolved actionable findings;
- all configured validation commands return exit code 0.

Wheel MAY invoke additional fresh review cycles up to `max_review_cycles` when a cycle returns a remediable non-PASS outcome or final validation still fails.

### FR-9 — Git commit

When commit is enabled, Wheel MUST commit only after final review and validation gates pass.

The commit message MUST support at least `{key}` and `{title}` expansion.

Wheel MUST record the resulting commit SHA for the completion comment/state.

### FR-10 — Work Item completion

After all gates pass, Wheel SHOULD add a completion comment containing:

- Work Item key;
- commit SHA (if committed);
- acceptance/review result summary;
- final validation result;
- review cycle count;
- outstanding finding count (must be zero for completion).

Wheel MUST then request `hamstik work close <KEY>`.

If close fails, Wheel MUST preserve state and MUST NOT silently select another Work Item.

### FR-11 — Durable state

Wheel MUST persist phase state under Git metadata, resolved using `git rev-parse --git-path hamstik-wheel/state.json`.

State MUST include at least:

- schema version;
- current Work Item key/title;
- baseline Git SHA;
- phase;
- review-cycle counter;
- completion counter for the active run;
- last error if present.

Persisted state MUST NOT include Hamstik credentials or model API keys.

### FR-12 — Resume

When state identifies an active Work Item, `once`, `run`, and `resume` MUST prefer resuming that Work Item over selecting a new one.

If repository state is incompatible with persisted state, Wheel MUST stop and explain the mismatch rather than reset or overwrite work.

### FR-13 — Doctor

`hamstik-wheel doctor` MUST check at least:

- current directory belongs to a Git repository;
- configuration parses;
- `git` executable is callable;
- `pi` executable is callable;
- implementation model can be resolved or appears in `pi --list-models` where practical;
- review model can be resolved or appears in `pi --list-models` where practical;
- `hamstik` executable is callable;
- `hamstik doctor --json --no-input` succeeds;
- repository cleanliness satisfies configuration when no active state exists;
- validation commands are configured.

### FR-14 — Exit behavior

Wheel MUST return non-zero when a run stops because of a failed required gate, malformed agent result, unavailable dependency, incompatible state, or Hamstik/Git failure.

No eligible Work Items is a successful terminal condition for `run`.

## 8. Non-functional requirements

### NFR-1 — Simplicity

The v0.1 implementation SHOULD remain a single Rust binary with no service/database dependency.

### NFR-2 — Observability

Commands and agent phases SHOULD stream useful terminal output. The current Work Item and phase MUST always be obvious in failure messages.

### NFR-3 — Performance

Wheel SHOULD avoid redundant Hamstik API traffic. It SHOULD use `hamstik work context` rather than manually issuing several individual reads. It SHOULD not keep long-lived LLM sessions between Work Items.

### NFR-4 — Security

Wheel MUST NOT read/store Hamstik PATs directly. It MUST avoid printing secrets from environment/configuration. It SHOULD rely on Hamstik CLI redaction and credential-store behavior.

### NFR-5 — Portability

Core process invocation SHOULD support Linux, macOS, and Windows where practical. Shell validation commands are repository-authored and may be platform-specific.

### NFR-6 — Determinism

Lifecycle transitions and completion decisions MUST be controlled by Wheel's state machine and exit-code/result gates, not by unstructured model prose.

## 9. State machine

```text
IDLE
  ↓ select
SELECTED
  ↓ context + start
CLAIMED
  ↓ Pi implementer
IMPLEMENTING
  ↓
PRE_REVIEW_VALIDATION
  ↓
REVIEWING ──────┐
  ↓             │ retry if allowed
FINAL_VALIDATION┘
  ↓ pass
COMMITTING
  ↓
CLOSING
  ↓
IDLE / next item
```

A phase transition MUST be persisted before beginning externally destructive/long-running work where doing so improves resume safety.

## 10. Acceptance criteria for v0.1 MVP

1. A user can initialize and configure Wheel in a Git repository without entering Hamstik credentials.
2. `doctor` confirms the required Git/Pi/Hamstik CLI boundaries and reports failures clearly.
3. `once` can discover one candidate through `hamstik work list --json` and load it through `hamstik work context --json`.
4. Wheel starts the Work Item through Hamstik CLI, launches the configured implementation Pi model, and preserves the baseline SHA.
5. Wheel runs repository validation and passes its output to an independent fresh reviewer Pi process using the configured review model.
6. The reviewer can modify the working tree to fix findings.
7. Wheel refuses completion when reviewer PASS is missing or final validation fails.
8. On successful gates, Wheel commits (when enabled), posts a summary comment, and closes the Work Item through Hamstik CLI.
9. State is stored outside the working tree and allows a failed/interrupted item to be resumed before new work is selected.
10. `run --max-items N` repeats sequentially and stops successfully when N items complete or no eligible items remain.
11. Neither Wheel's config nor state stores Hamstik credentials.
12. The project is Apache-2.0 licensed and suitable for publication under `blackboardstudios/hamstik-wheel`.

## 11. Future considerations

Possible later enhancements, intentionally excluded from v0.1:

- dedicated `agent-ready` label conventions;
- dependency/blocker-aware eligibility using authoritative Hamstik link semantics;
- branch-per-Work-Item behavior;
- pull request creation;
- worktrees and bounded parallelism;
- richer terminal dashboard;
- configurable role prompts;
- structured evidence attachments/comments in Hamstik;
- automatic `hamstik agent skill check/install` readiness;
- release packaging via cargo-dist/Homebrew/installer scripts similar to Hamstik CLI.
