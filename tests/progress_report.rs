// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use serde_json::json;
use std::{fs, path::Path, process::Command};

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run(root: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_hamstik-wheel"))
        .current_dir(root)
        .args(["--no-timestamps", "progress-report"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn event(kind: &str, key: &str, day: u32) -> String {
    json!({"kind":kind, "key":key, "title":"Example work", "timestamp":format!("2026-01-{day:02}T00:00:00Z")}).to_string() + "\n"
}

#[test]
fn empty_repository_report_needs_no_configuration_and_creates_no_metadata() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    assert!(run(dir.path()).contains("No recorded Hamstik Wheel progress"));
    assert!(!dir.path().join(".git/hamstik-wheel").exists());
}

#[test]
fn report_from_subdirectory_summarizes_latest_events_without_loading_configuration() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let logs = dir.path().join(".git/hamstik-wheel/logs");
    fs::create_dir_all(&logs).unwrap();
    let journal = event("started", "TEST-1", 1)
        + &event("completed", "TEST-1", 2)
        + &event("skipped", "TEST-2", 3)
        + &event("started", "TEST-2", 4);
    fs::write(logs.join("progress.jsonl"), &journal).unwrap();
    // An alias copied later is still weaker evidence than a lifecycle event.
    fs::create_dir(logs.join("TEST-1")).unwrap();
    fs::write(logs.join("TEST-1/implement.log"), "old transcript").unwrap();
    fs::write(dir.path().join(".hamstik-wheel.toml"), "invalid toml [").unwrap();
    let nested = dir.path().join("src/nested");
    fs::create_dir_all(&nested).unwrap();
    let output = run(&nested);
    assert!(
        output.contains("Completed TEST-1 — Example work"),
        "{output}"
    );
    assert!(output.contains("Started TEST-2 — Example work"), "{output}");
    assert!(!output.contains("Skipped TEST-2"));
    assert_eq!(output.matches("TEST-1").count(), 1);
    assert!(output.find("Completed TEST-1").unwrap() < output.find("Started TEST-2").unwrap());
    assert_eq!(
        fs::read_to_string(logs.join("progress.jsonl")).unwrap(),
        journal
    );
    assert!(!dir.path().join(".git/hamstik-wheel/runtime").exists());
}

#[test]
fn active_checkpoint_and_failures_are_reported_without_exposing_root_secrets() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let metadata = dir.path().join(".git/hamstik-wheel");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(dir.path().join(".env"), "EXAMPLE_SECRET=private-value\n").unwrap();
    let nested = dir.path().join("src");
    fs::create_dir(&nested).unwrap();
    let mut state = json!({
        "schemaVersion":1, "phase":"implementing",
        "current":{"key":"TEST-1","title":"Example private-value\nInjected line","baselineSha":"abc"},
        "reviewCycle":0, "completedThisRun":0, "lastError":null,
        "skipLedger":[{"key":"TEST-1","title":"Old attempt","reason":"model-timeout","model":null,"skippedAt":"2026-01-03T00:00:00Z"}],
        "updatedAt":"2026-01-01T00:00:00Z"
    });
    fs::write(metadata.join("state.json"), state.to_string()).unwrap();
    let output = run(&nested);
    assert!(
        output.contains("Started TEST-1 — Example [REDACTED]"),
        "{output}"
    );
    assert!(output.contains("process may be running or interrupted"));
    assert!(!output.contains("private-value") && !output.contains("\nInjected"));
    state["lastError"] = json!("connection failed: postgres://user:password@localhost/db");
    fs::write(metadata.join("state.json"), state.to_string()).unwrap();
    let output = run(&nested);
    assert!(output.contains("Failed TEST-1"), "{output}");
    assert!(!output.contains("user:password"));
    assert!(!output.contains("Completed TEST-1"));
}

#[test]
fn legacy_validation_and_model_claims_never_imply_completion() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let logs = dir.path().join(".git/hamstik-wheel/logs");
    fs::create_dir_all(logs.join("TEST-1")).unwrap();
    fs::create_dir_all(logs.join("TEST-2/history")).unwrap();
    fs::write(
        logs.join("TEST-1/final-validation-01.log"),
        "overall: PASS\nprivate validation output\n",
    )
    .unwrap();
    fs::write(
        logs.join("TEST-2/history/20260101T000000Z-0-implement.log"),
        "Completed TEST-2\nHAMSTIK_WHEEL_RESULT={\"status\":\"pass\"}\n",
    )
    .unwrap();
    let output = run(dir.path());
    assert!(output.contains("Validated TEST-1"), "{output}");
    assert!(output.contains("completion not recorded"));
    assert!(output.contains("Started TEST-2"));
    assert!(!output.contains("Completed TEST-"));
    assert!(!output.contains("private validation output"));
}

#[test]
fn worktree_report_uses_its_own_git_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("main");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q"]);
    git(
        &root,
        &[
            "-c",
            "user.name=Wheel Test",
            "-c",
            "user.email=wheel@example.test",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=",
            "commit",
            "--allow-empty",
            "-qm",
            "baseline",
        ],
    );
    let other = dir.path().join("linked");
    git(
        &root,
        &["worktree", "add", "-qb", "linked", other.to_str().unwrap()],
    );
    let metadata = Command::new("git")
        .current_dir(&other)
        .args(["rev-parse", "--git-path", "hamstik-wheel/logs"])
        .output()
        .unwrap();
    let logs = std::path::PathBuf::from(String::from_utf8(metadata.stdout).unwrap().trim());
    fs::create_dir_all(&logs).unwrap();
    fs::write(logs.join("progress.jsonl"), event("completed", "TEST-3", 1)).unwrap();
    assert!(run(&other).contains("Completed TEST-3"));
    assert!(run(&root).contains("No recorded Hamstik Wheel progress"));
}

#[test]
fn torn_journal_is_reported_and_later_records_remain_readable() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let logs = dir.path().join(".git/hamstik-wheel/logs");
    fs::create_dir_all(&logs).unwrap();
    fs::write(
        logs.join("progress.jsonl"),
        event("started", "TEST-1", 1)
            + "{\"kind\":\n"
            + &event("completed", "TEST-2", 2)
            + "{\"kind\":",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hamstik-wheel"))
        .current_dir(dir.path())
        .args(["--no-timestamps", "progress-report"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Completed TEST-2"));
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("Ignored 2 incomplete or invalid progress record(s)"));
}

#[test]
fn later_attempt_replaces_completion_even_when_the_clock_moves_backwards() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let logs = dir.path().join(".git/hamstik-wheel/logs");
    fs::create_dir_all(&logs).unwrap();
    fs::write(
        logs.join("progress.jsonl"),
        event("completed", "TEST-1", 3) + &event("started", "TEST-1", 1),
    )
    .unwrap();
    let output = run(dir.path());
    assert!(output.contains("Started TEST-1"), "{output}");
    assert!(!output.contains("Completed TEST-1"));
}
