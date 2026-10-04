// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

#![cfg(target_os = "linux")]

use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        fixture.git(&["init", "-q"]);
        fixture.git(&["config", "user.email", "wheel@test"]);
        fixture.git(&["config", "user.name", "wheel"]);
        fixture.git(&["config", "commit.gpgsign", "false"]);
        fixture.git(&["config", "core.hooksPath", "/dev/null"]);
        fs::create_dir_all(fixture.root().join(".git/bin")).unwrap();
        fixture.write(".git/bin/hamstik", HAMSTIK);
        fixture.write(".git/bin/pi", PI);
        for name in ["hamstik", "pi"] {
            fs::set_permissions(
                fixture.root().join(format!(".git/bin/{name}")),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let commands: Vec<_> = ["doctor", "commands", "sprint list", "work list", "work context", "work view", "work start", "work close", "work transition", "work comment add", "work edit"]
            .iter().map(|command| json!({"command": format!("hamstik {command}"), "capabilities":{"json":true,"noInput":true}})).collect();
        fixture.write(
            ".git/manifest.json",
            &json!({"commands":commands}).to_string(),
        );
        fixture.write(".git/sprints.json", "{\"items\":[]}");
        fixture.write(
            ".git/candidates.json",
            &json!([
                {"key":"TEST-1","title":"First","status":"todo","priority":"urgent"},
                {"key":"TEST-2","title":"Second","status":"todo","priority":"low"}
            ])
            .to_string(),
        );
        fixture.write(
            ".hamstik-wheel.toml",
            r#"
[hamstik]
cli_path = ".git/bin/hamstik"
[models]
implement = "implement"
review = "review"
[validation]
execution = "trusted_host"
commands = ["true"]
[loop]
on_failure = "skip"
provider_retries = 2
provider_retry_delay_seconds = 0
max_review_cycles = 1
[comments]
post_started = false
post_completed = false
"#,
        );
        fixture.write("base.txt", "baseline\n");
        fixture.git(&["add", "-A"]);
        fixture.git(&["commit", "-qm", "baseline"]);
        fixture
    }
    fn root(&self) -> &Path {
        self.dir.path()
    }
    fn write(&self, path: &str, text: &str) {
        fs::write(self.root().join(path), text).unwrap();
    }
    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.root().join(path)).unwrap()
    }
    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(self.root())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }
    fn command(&self, args: &[&str], mode: &str) -> Command {
        let path = format!(
            "{}:{}",
            self.root().join(".git/bin").display(),
            std::env::var("PATH").unwrap()
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_hamstik-wheel"));
        command
            .current_dir(self.root())
            .env("PATH", path)
            .env("TEST_MODE", mode)
            .args(args);
        command
    }
    fn run(&self, args: &[&str], mode: &str) -> Output {
        self.command(args, mode).output().unwrap()
    }
    fn state(&self) -> Value {
        serde_json::from_str(&self.read(".git/hamstik-wheel/state.json")).unwrap()
    }
    fn set_state(&self, state: &Value) {
        self.write(".git/hamstik-wheel/state.json", &state.to_string());
    }
    fn transcripts(&self, key: &str, name: &str) -> Vec<String> {
        fs::read_dir(
            self.root()
                .join(format!(".git/hamstik-wheel/logs/{key}/history")),
        )
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name().unwrap().to_string_lossy().ends_with(name)
                && !path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("legacy-")
        })
        .map(|path| fs::read_to_string(path).unwrap())
        .collect()
    }
}

const HAMSTIK: &str = r#"#!/bin/sh
if [ "$1" = '--version' ]; then echo test; exit 0; fi
shift 2
echo "$*" >> .git/hamstik-commands
case "$1 $2" in
  'commands ') cat .git/manifest.json;;
  'sprint list')
    if [ -f .git/sprint-error ]; then echo 'sprint service unavailable' >&2; exit 9; fi
    cat .git/sprints.json;;
  'work list')
    shift 2
    while [ "$#" -gt 0 ]; do
      if [ "$1" = '--sprint' ]; then
        if [ -f .git/sprint-items-error ]; then echo 'sprint items unavailable' >&2; exit 9; fi
        cat ".git/sprint-$2.json"; exit
      fi
      shift
    done
    cat .git/candidates.json;;
  'work context')
    if [ -f .git/claim-race ] && [ -f .git/status-read ]; then echo in_progress > ".git/status-$3"; fi
    if [ -f ".git/context-$3.json" ]; then cat ".git/context-$3.json"; else printf '{"item":{"key":"%s"},"links":{"items":[],"page":{"hasMore":false}}}\n' "$3"; fi;;
  'work start')
    echo "$3" >> .git/starts; echo in_progress > ".git/status-$3"
    if [ -f .git/start-lost-response ]; then echo 'response lost' >&2; exit 8; fi
    echo '{}';;
  'work view')
    if [ -f .git/view-fails ] && [ -f .git/starts ]; then echo 'status unavailable' >&2; exit 8; fi
    if [ -f .git/view-malformed ] && [ -f .git/starts ]; then echo '{}'; exit 0; fi
    if [ -f ".git/status-$3" ]; then status=$(cat ".git/status-$3"); else status=todo; fi
    touch .git/status-read
    printf '{"status":"%s"}\n' "$status";;
  'work transition')
    echo "$3 $4" >> .git/transitions
    if [ -f .git/transition-fails ]; then echo 'transition unavailable' >&2; exit 8; fi
    if [ ! -f .git/transition-noop ]; then echo "$4" > ".git/status-$3"; fi
    if [ -f .git/transition-lost-response ]; then echo 'response lost' >&2; exit 8; fi
    echo '{}';;
  'work close')
    if [ "$TEST_MODE" = 'close-fails' ]; then echo 'close unavailable' >&2; exit 1; fi
    echo "$3" >> .git/closes
    echo done > ".git/status-$3"
    if [ -f .git/drain-sprint ]; then echo '{"items":[]}' > ".git/sprint-$(cat .git/drain-sprint).json"; fi
    echo '{}';;
  'work comment') cat > /dev/null; echo '{}';;
  *) echo '{}';;
esac
"#;

const PI: &str = r#"#!/bin/sh
case " $* " in *' --wheel-guard-check '*) echo '{"type":"wheel_guard_ready","version":1}'; exit 0;; esac
case "$*" in *--help*|*--version*) echo test; exit 0;; esac
prompt=$(cat)
if [ "$prompt" = 'Reply with the single word: ok.' ]; then
  if [ "$TEST_MODE" = 'model-404' ]; then
    echo '{"type":"message_end","message":{"role":"assistant","model":"vendor/model:batch","provider":"openrouter","stopReason":"error","errorMessage":"404: vendor/model:batch cannot be used with the chat/completions endpoint"}}'
  elif [ "$TEST_MODE" = 'probe-429' ]; then
    echo '{"type":"message_end","message":{"role":"assistant","model":"resolved-probe","provider":"test-provider","stopReason":"error","errorMessage":"429: upstream rate-limited"}}'
  fi
  exit 0
fi
if [ -z "$prompt" ]; then exit 0; fi
model=$2
if [ "$TEST_MODE" = 'missing-guard' ]; then
  echo '{"type":"wheel_result","result":{"status":"pass","summary":"unsafe","findings":[],"unverified_checks":[]}}'; exit 0
fi
if [ "$TEST_MODE" = 'slow-log' ]; then
  echo '{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"postgresql://someone:password@localhost/test"}]}}'
  echo $$ > .git/slow-pid
  sleep 30
fi
echo '{"type":"wheel_guard_ready","version":1}'
printf '%s\n' "$prompt" > ".git/last-$model-prompt"
echo "$model" >> .git/sessions
if [ "$model" = 'implement' ]; then
  case "$prompt" in *'Work Item: TEST-1 '*) key=TEST-1;; *) key=TEST-2;; esac
  echo changed > "$key.txt"
  if [ -f .git/status-during-implementation ]; then cat .git/status-during-implementation > ".git/status-$key"; fi
  if [ "$TEST_MODE" = 'skip' ] && [ "$key" = 'TEST-1' ]; then
    echo '{"type":"wheel_result","result":{"findings":["blocked"],"unverified_checks":[],"status":"blocked","summary":"item blocked"}}'
  elif [ "$TEST_MODE" = 'unverified' ]; then
    echo '{"type":"wheel_result","result":{"findings":[],"unverified_checks":["populated migration test"],"status":"ready_for_review","summary":"implemented"}}'
  else
    echo '{"type":"wheel_result","result":{"findings":[],"unverified_checks":[],"status":"ready_for_review","summary":"implemented"}}'
  fi
else
  if [ "$TEST_MODE" = 'review-migration' ]; then echo migration > migration.sql; fi
  if [ "$TEST_MODE" = 'rate-limit' ] || { [ "$TEST_MODE" = 'flaky-provider' ] && [ ! -f .git/provider-retried ]; }; then
    touch .git/provider-retried
    echo '{"type":"message_end","message":{"role":"assistant","model":"resolved-review","provider":"test-provider","stopReason":"error","errorMessage":"429: upstream rate-limited"}}'
    echo '{"type":"auto_retry_end","success":false,"finalError":"429: upstream rate-limited"}'
  elif [ "$TEST_MODE" = 'unauthorized' ]; then
    echo '{"type":"message_end","message":{"role":"assistant","model":"resolved-review","provider":"test-provider","stopReason":"error","errorMessage":"401: invalid API key"}}'
  elif [ "$TEST_MODE" = 'no-marker' ]; then
    echo 'No marker'
  elif [ "$TEST_MODE" = 'unverified-review' ]; then
    echo '{"type":"wheel_result","result":{"findings":[],"unverified_checks":["populated migration test"],"status":"pass","summary":"reviewed"}}'
  else
    echo '{"type":"wheel_result","result":{"findings":[],"unverified_checks":[],"status":"pass","summary":"reviewed"}}'
  fi
fi
"#;

#[test]
fn preflight_fails_when_model_probe_reports_non_retryable_provider_error() {
    let f = Fixture::new();
    let output = f.run(&["once"], "model-404");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Implementation model probe failed"));
    assert!(stderr.contains("404: vendor/model:batch cannot be used"));
    assert!(!f.root().join(".git/sessions").exists());
    assert!(!f.root().join(".git/starts").exists());
}

#[test]
fn retryable_probe_error_passes_preflight_and_run_proceeds() {
    let f = Fixture::new();
    let output = f.run(&["once"], "probe-429");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(f.read(".git/closes"), "TEST-1\n");
}

#[test]
fn provider_outage_preserves_review_and_resume_does_not_reimplement() {
    let f = Fixture::new();
    let baseline = f.git(&["rev-parse", "HEAD"]);
    let output = f.run(&["--log-file", ".git/run.log", "once"], "rate-limit");
    assert!(!output.status.success());
    assert!(f
        .read(".git/run.log")
        .contains("provider unavailable; active checkpoint preserved"));
    let state = f.state();
    assert_eq!(state["phase"], "reviewing");
    assert_eq!(state["current"]["previousStatus"], "todo");
    assert_eq!(state["current"]["needsReclaim"], true);
    assert_eq!(f.read(".git/status-TEST-1").trim(), "todo");
    assert_eq!(state["current"]["key"], "TEST-1");
    assert!(state["lastError"]
        .as_str()
        .unwrap()
        .contains("resolved-review"));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), baseline);
    assert!(f.root().join("TEST-1.txt").exists());
    assert_eq!(
        f.read(".git/sessions"),
        "implement\nreview\nreview\nreview\n"
    );
    assert_eq!(f.transcripts("TEST-1", "review-01.log").len(), 3);
    let output = f.run(&["resume"], "success");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(f.read(".git/sessions").matches("implement").count(), 1);
    assert_eq!(f.read(".git/closes"), "TEST-1\n");
    assert_eq!(f.state()["phase"], "idle");
    assert_eq!(f.read(".git/starts"), "TEST-1\nTEST-1\n");
    assert_eq!(f.transcripts("TEST-1", "review-01.log").len(), 4);
}

#[test]
fn transient_provider_recovery_completes_and_authentication_failure_does_not_retry() {
    let f = Fixture::new();
    assert!(f.run(&["once"], "flaky-provider").status.success());
    assert_eq!(f.read(".git/sessions"), "implement\nreview\nreview\n");
    assert_eq!(f.read(".git/closes"), "TEST-1\n");
    let f = Fixture::new();
    assert!(!f.run(&["once"], "unauthorized").status.success());
    assert_eq!(f.read(".git/sessions"), "implement\nreview\n");
    assert_eq!(f.state()["phase"], "reviewing");
}

#[test]
fn rules_are_reevaluated_for_files_added_during_review() {
    let f = Fixture::new();
    let config = f.read(".hamstik-wheel.toml")
        + r#"
[[validation.rules]]
path_prefixes = ["migration.sql"]
commands = ["echo required-migration-check; exit 1"]
"#;
    f.write(".hamstik-wheel.toml", &config);
    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "validation rule"]);
    assert!(f.run(&["once"], "review-migration").status.success());
    assert!(!f
        .read(".git/hamstik-wheel/logs/TEST-1/pre-review-validation.log")
        .contains("required-migration-check"));
    assert!(f
        .read(".git/hamstik-wheel/logs/TEST-1/final-validation-01.log")
        .contains("required-migration-check"));
    assert!(!f.root().join(".git/closes").exists());
}

#[test]
fn skip_advances_and_cooldown_does_not_hide_other_candidates() {
    let f = Fixture::new();
    let output = f.run(&["run", "--max-items", "2"], "skip");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(f.read(".git/starts"), "TEST-1\nTEST-2\n");
    assert_eq!(f.read(".git/closes"), "TEST-2\n");
    let progress = f.run(&["--no-timestamps", "progress-report"], "skip");
    assert!(progress.status.success());
    let progress = String::from_utf8(progress.stdout).unwrap();
    assert!(progress.contains("Skipped TEST-1 — First"), "{progress}");
    assert!(progress.contains("Completed TEST-2 — Second"), "{progress}");
    assert_eq!(f.state()["completedThisRun"], 1);
    assert_eq!(f.state()["skipLedger"][0]["wipBranch"], "wheel/wip/TEST-1");
    assert_eq!(
        f.state()["skipLedger"][0]["wipSha"],
        f.git(&["rev-parse", "wheel/wip/TEST-1"])
    );
    // A fresh invocation still considers the second item despite TEST-1's cooldown.
    f.write(".git/starts", "");
    f.write(".git/status-TEST-2", "todo\n"); // explicitly reopened for another attempt
    let output = f.run(&["once"], "skip");
    assert!(output.status.success());
    assert_eq!(f.read(".git/starts"), "TEST-2\n");
}

#[test]
fn failed_close_is_never_recorded_as_completed_even_after_validation_passes() {
    let f = Fixture::new();
    assert!(f.run(&["once"], "close-fails").status.success());
    assert!(f
        .read(".git/hamstik-wheel/logs/TEST-1/final-validation-01.log")
        .starts_with("overall: PASS"));
    let journal = f.read(".git/hamstik-wheel/logs/progress.jsonl");
    let events: Vec<Value> = journal
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(|e| e["kind"] == "started"));
    assert!(events.iter().any(|e| e["kind"] == "failed"));
    assert!(!events.iter().any(|e| e["kind"] == "completed"));
    let output = f.run(&["--no-timestamps", "progress-report"], "close-fails");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Skipped TEST-1"));
}

#[test]
fn failed_protocol_retries_and_repeated_item_attempts_keep_every_transcript() {
    let f = Fixture::new();
    assert!(f.run(&["once"], "no-marker").status.success());
    let mut state = f.state();
    state["skipLedger"][0]["skippedAt"] = json!("2020-01-01T00:00:00Z");
    f.set_state(&state);
    assert!(f.run(&["once"], "no-marker").status.success());
    let logs = f.transcripts("TEST-1", "review-01.log");
    assert_eq!(logs.len(), 4);
    assert!(logs.iter().all(|log| log.contains("No marker")));
    assert_eq!(f.transcripts("TEST-1", "implement.log").len(), 2);
}

#[test]
fn unverified_checks_prevent_completion_even_with_success_markers() {
    for mode in ["unverified", "unverified-review"] {
        let f = Fixture::new();
        assert!(f.run(&["once"], mode).status.success());
        assert!(!f.root().join(".git/closes").exists());
        assert_eq!(f.state()["completedThisRun"], 0);
    }
}

#[test]
fn conditional_validation_failure_prevents_close() {
    let f = Fixture::new();
    let config = f.read(".hamstik-wheel.toml")
        + r#"
[[validation.rules]]
path_prefixes = ["TEST-"]
commands = ["echo database-unavailable; exit 1"]
"#;
    f.write(".hamstik-wheel.toml", &config);
    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "validation rule"]);
    let output = f.run(&["once"], "success");
    assert!(output.status.success());
    assert!(!f.root().join(".git/closes").exists());
    let log = f.read(".git/hamstik-wheel/logs/TEST-1/final-validation-01.log");
    assert!(log.contains("overall: FAIL"));
    assert!(log.contains("database-unavailable"));
}

#[test]
fn legacy_state_recovers_latest_wip_across_deleted_suffixes() {
    let f = Fixture::new();
    let baseline = f.git(&["rev-parse", "HEAD"]);
    f.write("old.txt", "older");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "old work"]);
    f.git(&["branch", "wheel/wip/TEST-1"]);
    f.write("new.txt", "newer");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "new work"]);
    let latest = f.git(&["rev-parse", "HEAD"]);
    f.git(&["branch", "wheel/wip/TEST-1-3"]);
    f.git(&["reset", "--hard", &baseline]);
    assert!(f.run(&["once"], "skip").status.success());
    let prompt = f.read(".git/last-implement-prompt");
    assert!(prompt.contains(&format!(
        "Latest preserved work: commit {latest}, branch wheel/wip/TEST-1-3"
    )));
    assert_eq!(
        f.state()["skipLedger"][0]["wipBranch"],
        "wheel/wip/TEST-1-4"
    );
}

const SPRINT_ID: &str = "11111111-1111-1111-1111-111111111111";

impl Fixture {
    fn active_sprint(&self, items: Value) {
        self.write(
            ".git/sprints.json",
            &json!({"items":[{
                "id":SPRINT_ID,"name":"Current sprint","state":"active","archivedAt":null
            }]})
            .to_string(),
        );
        self.write(
            &format!(".git/sprint-{SPRINT_ID}.json"),
            &json!({"items":items}).to_string(),
        );
    }
}

#[test]
fn active_sprint_work_precedes_higher_priority_project_work() {
    let f = Fixture::new();
    let config = f
        .read(".hamstik-wheel.toml")
        .replace("[hamstik]", "[hamstik]\nlabel_names = ['agent-ready']");
    f.write(".hamstik-wheel.toml", &config);
    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "label filter"]);
    f.active_sprint(json!([{"key":"TEST-2","title":"Second","status":"todo","priority":"low"}]));
    let output = f.run(&["once"], "success");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(f.read(".git/starts"), "TEST-2\n");
    let commands = f.read(".git/hamstik-commands");
    assert!(commands.contains("sprint list --all"));
    let lists: Vec<_> = commands
        .lines()
        .filter(|line| line.starts_with("work list "))
        .collect();
    assert_eq!(
        lists.len(),
        1,
        "no project-wide fallback when sprint work exists"
    );
    assert!(lists[0].contains(&format!("--sprint {SPRINT_ID} --all")));
    assert!(lists[0].contains("--status todo --type task --type bug --type story --type feature"));
    assert!(lists[0].contains("--label-name agent-ready"));
}

#[test]
fn sprint_is_rechecked_after_completion_and_empty_sprint_falls_back() {
    let f = Fixture::new();
    f.active_sprint(json!([{"key":"TEST-2","title":"Second","status":"todo","priority":"low"}]));
    f.write(".git/drain-sprint", SPRINT_ID);
    let output = f.run(&["run", "--max-items", "2"], "success");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(f.read(".git/starts"), "TEST-2\nTEST-1\n");
    assert_eq!(f.read(".git/closes"), "TEST-2\nTEST-1\n");
    assert_eq!(f.state()["completedThisRun"], 2);
    assert_eq!(
        f.read(".git/hamstik-commands")
            .matches("sprint list --all")
            .count(),
        2
    );
}

#[test]
fn preflight_requires_sprint_discovery_capability() {
    let f = Fixture::new();
    let mut manifest: Value = serde_json::from_str(&f.read(".git/manifest.json")).unwrap();
    manifest["commands"]
        .as_array_mut()
        .unwrap()
        .retain(|entry| entry["command"] != "hamstik sprint list");
    f.write(".git/manifest.json", &manifest.to_string());
    let output = f.run(&["once"], "success");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("required command `hamstik sprint list`")
    );
    assert!(!f.root().join(".git/starts").exists());
}

#[test]
fn empty_sprint_and_no_active_sprint_fall_back_to_existing_selection() {
    for active in [false, true] {
        let f = Fixture::new();
        if active {
            f.active_sprint(json!([]));
        } else {
            f.write(
                ".git/sprints.json",
                &json!({"items":[
                    {"id":"future","state":"future"},
                    {"id":"done","state":"done"},
                    {"id":"archived","state":"active","archivedAt":"2026-01-01"}
                ]})
                .to_string(),
            );
        }
        assert!(f.run(&["once"], "success").status.success());
        assert_eq!(f.read(".git/starts"), "TEST-1\n");
        let commands = f.read(".git/hamstik-commands");
        let lists: Vec<_> = commands
            .lines()
            .filter(|line| line.starts_with("work list "))
            .collect();
        assert_eq!(lists.len(), if active { 2 } else { 1 });
        assert!(!lists.last().unwrap().contains("--sprint"));
    }
}

#[test]
fn skipped_and_cooling_sprint_items_allow_project_fallback() {
    let f = Fixture::new();
    f.active_sprint(json!([{"key":"TEST-1","title":"First","status":"todo","priority":"urgent"}]));
    let output = f.run(&["run", "--max-items", "2"], "skip");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(f.read(".git/starts"), "TEST-1\nTEST-2\n");
    assert_eq!(f.read(".git/closes"), "TEST-2\n");
    assert!(String::from_utf8_lossy(&output.stdout).contains("No eligible active-sprint work"));
    f.write(".git/starts", "");
    f.write(".git/status-TEST-2", "todo\n"); // explicitly reopened for another attempt
    assert!(f.run(&["once"], "skip").status.success());
    assert_eq!(f.read(".git/starts"), "TEST-2\n");
}

#[test]
fn sprint_api_errors_do_not_silently_select_project_work() {
    for marker in [".git/sprint-error", ".git/sprint-items-error"] {
        let f = Fixture::new();
        f.active_sprint(json!([]));
        f.write(marker, "fail");
        assert!(!f.run(&["once"], "success").status.success());
        assert!(!f.root().join(".git/starts").exists());
        assert!(!f.root().join(".git/sessions").exists());
    }
}

#[test]
fn resumed_item_is_not_replaced_when_sprint_changes() {
    let f = Fixture::new();
    assert!(!f.run(&["once"], "rate-limit").status.success());
    f.active_sprint(json!([{"key":"TEST-2","status":"todo","priority":"urgent"}]));
    // Resume must not need sprint discovery at all.
    f.write(".git/sprint-error", "fail");
    assert!(f.run(&["resume"], "success").status.success());
    assert_eq!(f.read(".git/starts"), "TEST-1\nTEST-1\n");
    assert_eq!(f.read(".git/closes"), "TEST-1\n");
}

#[test]
fn skip_restores_actual_preclaim_status_instead_of_assuming_todo() {
    let f = Fixture::new();
    // The candidate list is stale; the server status at claim time wins.
    f.write(".git/status-TEST-1", "backlog\n");
    assert!(f.run(&["once"], "skip").status.success());
    assert_eq!(f.read(".git/transitions"), "TEST-1 backlog\n");
    assert_eq!(f.read(".git/status-TEST-1").trim(), "backlog");
    assert_eq!(f.state()["phase"], "idle");
}

#[test]
fn failed_skip_restoration_retains_checkpoint_and_recovers_without_models() {
    for failure in [
        "transition-fails",
        "transition-noop",
        "transition-lost-response",
        "view-fails",
        "view-malformed",
    ] {
        let f = Fixture::new();
        f.write(&format!(".git/{failure}"), "fail");
        let output = f.run(&["run", "--max-items", "2"], "skip");
        assert!(!output.status.success(), "{failure}");
        let state = f.state();
        assert_eq!(state["phase"], "skipping", "{failure}");
        assert_eq!(state["current"]["key"], "TEST-1");
        assert_eq!(state["current"]["previousStatus"], "todo");
        assert_eq!(state["skipLedger"].as_array().unwrap().len(), 0);
        assert_eq!(f.read(".git/starts"), "TEST-1\n");
        fs::remove_file(f.root().join(format!(".git/{failure}"))).unwrap();
        // This mode fails preflight if called. Recovery must finish without it.
        let output = f.run(&["resume"], "model-404");
        assert!(
            output.status.success(),
            "{failure}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(f.state()["phase"], "idle");
        assert_eq!(f.read(".git/status-TEST-1").trim(), "todo");
        assert_eq!(f.read(".git/starts"), "TEST-1\n");
        assert!(!f.root().join(".git/closes").exists());
    }
}

#[test]
fn halt_releases_claim_without_discarding_resumable_work() {
    let f = Fixture::new();
    f.write(
        ".hamstik-wheel.toml",
        &f.read(".hamstik-wheel.toml")
            .replace("on_failure = \"skip\"", "on_failure = \"halt\""),
    );
    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "halt mode"]);
    f.write(".git/status-TEST-1", "backlog\n");
    assert!(!f.run(&["once"], "skip").status.success());
    assert_eq!(f.read(".git/status-TEST-1").trim(), "backlog");
    assert_eq!(f.state()["phase"], "implementing");
    assert_eq!(f.state()["current"]["needsReclaim"], true);
    assert!(f.root().join("TEST-1.txt").exists());
    let report = f.run(&["--no-timestamps", "progress-report"], "success");
    assert!(String::from_utf8_lossy(&report.stdout).contains("Paused TEST-1"));
    assert!(f.run(&["resume"], "success").status.success());
    assert_eq!(f.read(".git/starts"), "TEST-1\nTEST-1\n");
    assert_eq!(f.read(".git/status-TEST-1").trim(), "done");
}

#[test]
fn provider_failure_keeps_release_intent_until_verified_before_another_probe() {
    let f = Fixture::new();
    f.write(".git/transition-fails", "fail");
    assert!(!f.run(&["once"], "rate-limit").status.success());
    assert_eq!(f.state()["current"]["releasePending"], true);
    assert_eq!(f.read(".git/status-TEST-1").trim(), "in_progress");
    fs::remove_file(f.root().join(".git/transition-fails")).unwrap();
    let output = f.run(&["resume"], "model-404");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("model probe failed"));
    assert_eq!(f.read(".git/status-TEST-1").trim(), "todo");
    assert_eq!(f.state()["current"]["releasePending"], false);
    assert_eq!(f.state()["current"]["needsReclaim"], true);
    assert_eq!(f.read(".git/starts"), "TEST-1\n");
}

#[test]
fn legacy_checkpoint_does_not_guess_previous_status_and_preserves_manual_transitions() {
    let f = Fixture::new();
    f.write(".git/transition-fails", "fail");
    assert!(!f.run(&["once"], "skip").status.success());
    fs::remove_file(f.root().join(".git/transition-fails")).unwrap();
    let mut state = f.state();
    state["current"]
        .as_object_mut()
        .unwrap()
        .remove("previousStatus");
    f.set_state(&state);
    f.write(".git/transitions", "");
    let output = f.run(&["resume"], "success");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("legacy checkpoint has no previous status"));
    assert_eq!(f.state()["phase"], "skipping");
    assert_eq!(f.read(".git/transitions"), "");
    // User resolves the unknown historical status; Wheel must not overwrite it.
    f.write(".git/status-TEST-1", "backlog\n");
    assert!(f.run(&["resume"], "model-404").status.success());
    assert_eq!(f.state()["phase"], "idle");
    assert_eq!(f.read(".git/status-TEST-1").trim(), "backlog");
    assert_eq!(f.read(".git/transitions"), "");
}

#[test]
fn failed_resume_preflight_releases_an_interrupted_claim_before_returning() {
    let f = Fixture::new();
    assert!(!f.run(&["once"], "rate-limit").status.success());
    // Simulate a process dying during review with a durable pre-claim status.
    let mut state = f.state();
    state["current"]["needsReclaim"] = json!(false);
    state["current"]["claimAttempted"] = json!(true);
    state["current"]["claimConfirmed"] = json!(true);
    state["lastError"] = Value::Null;
    f.set_state(&state);
    f.write(".git/status-TEST-1", "in_progress\n");
    let output = f.run(&["resume"], "model-404");
    assert!(!output.status.success());
    assert_eq!(f.read(".git/status-TEST-1").trim(), "todo");
    assert_eq!(f.state()["current"]["needsReclaim"], true);
    assert_eq!(f.state()["phase"], "reviewing");
    assert!(f.root().join("TEST-1.txt").exists());
}

#[test]
fn skip_does_not_overwrite_a_status_changed_by_someone_else() {
    let f = Fixture::new();
    f.write(".git/status-during-implementation", "done\n");
    assert!(f.run(&["once"], "skip").status.success());
    assert_eq!(f.read(".git/status-TEST-1").trim(), "done");
    assert!(!f.root().join(".git/transitions").exists());
    assert_eq!(f.state()["phase"], "idle");
}

#[test]
fn concurrent_claim_is_not_adopted_or_released_by_wheel() {
    let f = Fixture::new();
    f.write(".git/claim-race", "race");
    let output = f.run(&["once"], "success");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ownership is unknown"));
    assert!(!f.root().join(".git/starts").exists());
    assert!(!f.root().join(".git/sessions").exists());
    assert!(!f.root().join(".git/transitions").exists());
    assert_eq!(f.read(".git/status-TEST-1").trim(), "in_progress");
    assert_eq!(f.state()["current"]["claimAttempted"], false);
}

#[test]
fn ambiguous_start_response_retains_cleanup_without_releasing_an_unproven_claim() {
    let f = Fixture::new();
    f.write(".git/start-lost-response", "fail");
    f.write(".git/status-TEST-1", "backlog\n");
    assert!(!f.run(&["once"], "success").status.success());
    assert_eq!(f.read(".git/status-TEST-1").trim(), "in_progress");
    assert_eq!(f.state()["phase"], "skipping");
    assert_eq!(f.state()["current"]["claimAttempted"], true);
    assert_eq!(f.state()["current"]["claimConfirmed"], false);
    assert!(!f.root().join(".git/transitions").exists());
    assert!(!f.root().join(".git/sessions").exists());
    f.write(".git/status-TEST-1", "backlog\n");
    assert!(f.run(&["resume"], "model-404").status.success());
    assert_eq!(f.state()["phase"], "idle");
}

#[test]
fn legacy_pause_does_not_adopt_done_as_a_status_to_reclaim() {
    let f = Fixture::new();
    assert!(!f.run(&["once"], "rate-limit").status.success());
    let mut state = f.state();
    state["current"]
        .as_object_mut()
        .unwrap()
        .remove("previousStatus");
    state["current"]["needsReclaim"] = json!(false);
    f.set_state(&state);
    f.write(".git/status-TEST-1", "done\n");
    assert!(!f.run(&["resume"], "model-404").status.success());
    assert!(f.state()["current"]["previousStatus"].is_null());
    let output = f.run(&["resume"], "success");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(f.read(".git/starts"), "TEST-1\n");
    assert_eq!(f.read(".git/status-TEST-1").trim(), "done");
    assert!(!f.root().join(".git/closes").exists());
}

#[test]
fn multiple_active_sprints_share_the_existing_candidate_order() {
    let f = Fixture::new();
    let second = "22222222-2222-2222-2222-222222222222";
    f.active_sprint(json!([{"key":"TEST-2","status":"todo","priority":"low"}]));
    f.write(
        ".git/sprints.json",
        &json!({"items":[
            {"id":SPRINT_ID,"state":"active"},{"id":second,"state":"active"}
        ]})
        .to_string(),
    );
    f.write(
        &format!(".git/sprint-{second}.json"),
        &json!({"items":[
            {"key":"TEST-1","status":"todo","priority":"urgent"}
        ]})
        .to_string(),
    );
    assert!(f.run(&["once"], "success").status.success());
    assert_eq!(f.read(".git/starts"), "TEST-1\n");
    let commands = f.read(".git/hamstik-commands");
    let lists: Vec<_> = commands
        .lines()
        .filter(|line| line.starts_with("work list "))
        .collect();
    assert_eq!(lists.len(), 2);
    assert!(lists.iter().all(|line| line.contains("--sprint")));
}

#[test]
fn unfinished_dependency_is_deferred_before_claim_and_does_not_hide_ready_work() {
    let f = Fixture::new();
    f.write(
        ".git/context-TEST-1.json",
        &json!({"item":{"key":"TEST-1"},"links":{"items":[
        {"relation":"blocked_by","otherWorkItem":{"key":"TEST-2","status":"todo"}}
    ],"page":{"hasMore":false}}})
        .to_string(),
    );
    let output = f.run(&["once"], "pass");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(f.read(".git/starts").trim(), "TEST-2");
    assert_eq!(f.read(".git/closes").trim(), "TEST-2");
    assert!(String::from_utf8_lossy(&output.stdout).contains("unfinished prerequisites"));
}

#[test]
fn incomplete_dependency_evidence_stops_before_claim() {
    let f = Fixture::new();
    f.write(
        ".git/context-TEST-1.json",
        &json!({"links":{"items":[],"page":{"hasMore":true}}}).to_string(),
    );
    assert!(!f.run(&["once"], "pass").status.success());
    assert!(!f.root().join(".git/starts").exists());
}

#[test]
fn missing_tool_guard_never_closes_or_skips_the_item() {
    let f = Fixture::new();
    assert!(!f.run(&["once"], "missing-guard").status.success());
    assert!(!f.root().join(".git/closes").exists());
    assert_eq!(f.state()["current"]["key"], "TEST-1");
    assert_eq!(f.state()["skipLedger"].as_array().unwrap().len(), 0);
}

#[test]
fn required_rule_coverage_blocks_completion_when_no_rule_covers_a_change() {
    let f = Fixture::new();
    let config = f.read(".hamstik-wheel.toml").replace(
        "commands = [\"true\"]",
        "commands = [\"true\"]\nrequire_rules_for = [\"TEST-\"]",
    );
    f.write(".hamstik-wheel.toml", &config);
    f.git(&["add", ".hamstik-wheel.toml"]);
    f.git(&["commit", "-qm", "validation policy"]);
    assert!(f.run(&["once"], "pass").status.success());
    assert!(!f.root().join(".git/closes").exists());
    assert_eq!(f.state()["skipLedger"].as_array().unwrap().len(), 1);
}

#[test]
fn live_transcripts_are_redacted_and_survive_wheel_termination() {
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    let f = Fixture::new();
    let mut child = f
        .command(&["once"], "slow-log")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !f.root().join(".git/slow-pid").exists() && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let logs = f.transcripts("TEST-1", "implement.log");
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(logs.len(), 1);
    assert!(logs[0].contains("[REDACTED]@localhost/test"));
    assert!(!logs[0].contains("someone:password"));
    assert!(!f.transcripts("TEST-1", "implement.log")[0].is_empty());
}

#[test]
fn validation_defaults_to_isolation_even_for_agent_editable_scripts() {
    let f = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let sentinel = outside.path().join("sentinel");
    fs::write(&sentinel, "original").unwrap();
    let config = f
        .read(".hamstik-wheel.toml")
        .replace("execution = \"trusted_host\"\n", "")
        .replace("commands = [\"true\"]", "commands = [\"sh validate.sh\"]");
    f.write(".hamstik-wheel.toml", &config);
    f.write(
        "validate.sh",
        &format!(
            "set -e\ntest -z \"${{WHEEL_TEST_SECRET:-}}\"\nmkdir -p '{}'\nprintf changed > '{}'\n",
            outside.path().display(),
            sentinel.display()
        ),
    );
    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "isolated validation"]);
    let result = f
        .command(&["once"], "pass")
        .env("WHEEL_TEST_SECRET", "host-secret")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        f.root().join(".git/closes").exists(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    assert_eq!(fs::read_to_string(sentinel).unwrap(), "original");
}
