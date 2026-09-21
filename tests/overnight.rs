// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

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
        let commands: Vec<_> = ["doctor", "commands", "sprint list", "work list", "work context", "work start", "work close", "work transition", "work comment add", "work edit"]
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
    fn run(&self, args: &[&str], mode: &str) -> Output {
        let path = format!(
            "{}:{}",
            self.root().join(".git/bin").display(),
            std::env::var("PATH").unwrap()
        );
        Command::new(env!("CARGO_BIN_EXE_hamstik-wheel"))
            .current_dir(self.root())
            .env("PATH", path)
            .env("TEST_MODE", mode)
            .args(args)
            .output()
            .unwrap()
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
  'work context') printf '{"item":{"key":"%s"}}\n' "$3";;
  'work start') echo "$3" >> .git/starts; echo '{}';;
  'work view') echo '{"status":"in_progress"}';;
  'work close')
    echo "$3" >> .git/closes
    if [ -f .git/drain-sprint ]; then echo '{"items":[]}' > ".git/sprint-$(cat .git/drain-sprint).json"; fi
    echo '{}';;
  'work comment') cat > /dev/null; echo '{}';;
  *) echo '{}';;
esac
"#;

const PI: &str = r#"#!/bin/sh
case "$*" in *--help*|*--version*) echo test; exit 0;; esac
prompt=$(cat)
if [ -z "$prompt" ]; then exit 0; fi
model=$2
printf '%s\n' "$prompt" > ".git/last-$model-prompt"
echo "$model" >> .git/sessions
if [ "$model" = 'implement' ]; then
  case "$prompt" in *'Work Item: TEST-1 '*) key=TEST-1;; *) key=TEST-2;; esac
  echo changed > "$key.txt"
  if [ "$TEST_MODE" = 'skip' ] && [ "$key" = 'TEST-1' ]; then
    echo 'HAMSTIK_WHEEL_RESULT={"status":"blocked","summary":"item blocked","findings":["blocked"]}'
  elif [ "$TEST_MODE" = 'unverified' ]; then
    echo 'HAMSTIK_WHEEL_RESULT={"status":"ready_for_review","summary":"implemented","unverified_checks":["populated migration test"]}'
  else
    echo 'HAMSTIK_WHEEL_RESULT={"status":"ready_for_review","summary":"implemented","findings":[]}'
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
    echo 'HAMSTIK_WHEEL_RESULT={"status":"pass","summary":"reviewed","unverified_checks":["populated migration test"]}'
  else
    echo 'HAMSTIK_WHEEL_RESULT={"status":"pass","summary":"reviewed","findings":[]}'
  fi
fi
"#;

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
    assert_eq!(f.state()["completedThisRun"], 1);
    assert_eq!(f.state()["skipLedger"][0]["wipBranch"], "wheel/wip/TEST-1");
    assert_eq!(
        f.state()["skipLedger"][0]["wipSha"],
        f.git(&["rev-parse", "wheel/wip/TEST-1"])
    );
    // A fresh invocation still considers the second item despite TEST-1's cooldown.
    f.write(".git/starts", "");
    let output = f.run(&["once"], "skip");
    assert!(output.status.success());
    assert_eq!(f.read(".git/starts"), "TEST-2\n");
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
    assert_eq!(f.read(".git/starts"), "TEST-1\n");
    assert_eq!(f.read(".git/closes"), "TEST-1\n");
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
