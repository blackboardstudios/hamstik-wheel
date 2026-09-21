// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{path::Path, process::Command};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::logging::Logger;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationKind {
    Inspection,
    Final,
}

impl ValidationKind {
    fn tag(self) -> &'static str {
        match self {
            Self::Inspection => "inspect",
            Self::Final => "validate",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationCommandResult {
    pub command: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    pub passed: bool,
    pub commands: Vec<ValidationCommandResult>,
}

impl ValidationReport {
    /// Full evidence: per-command exit, stdout, and stderr. Used for the
    /// per-Work-Item validation logs.
    pub fn evidence(&self) -> String {
        let mut out = format!("overall: {}\n", if self.passed { "PASS" } else { "FAIL" });
        for item in &self.commands {
            out.push_str(&format!(
                "\n$ {}\nexit: {:?}\n",
                item.command, item.exit_code
            ));
            if !item.stdout.is_empty() {
                out.push_str("stdout:\n");
                out.push_str(&item.stdout);
                out.push('\n');
            }
            if !item.stderr.is_empty() {
                out.push_str("stderr:\n");
                out.push_str(&item.stderr);
                out.push('\n');
            }
        }
        out
    }

    /// Bounded digest for review prompts: per-command status plus the tail of
    /// each stream, capped so the review prompt cannot overflow the model
    /// context. The full output is available in the validation log file.
    pub fn evidence_digest(&self, max_chars_per_stream: usize) -> String {
        let mut out = format!(
            "overall: {}. Only the configured commands listed below have been run by Hamstik Wheel. PASS does not establish that every repository-required check was included; identify missing required checks during review.\n(full untruncated output is saved in .git/hamstik-wheel/logs/<KEY>/pre-review-validation.log)\n",
            if self.passed { "PASS" } else { "FAIL" }
        );
        for item in &self.commands {
            let status = if item.exit_code == Some(0) {
                "PASS"
            } else {
                "FAIL"
            };
            out.push_str(&format!("\n$ {} -> {}\n", item.command, status));
            for (label, text) in [("stdout", &item.stdout), ("stderr", &item.stderr)] {
                if text.trim().is_empty() {
                    continue;
                }
                let (body, truncated) = tail_chars(text, max_chars_per_stream);
                out.push_str(&format!(
                    "{label}{}:\n{body}\n",
                    if truncated {
                        " (truncated, tail only)"
                    } else {
                        ""
                    }
                ));
            }
        }
        out
    }
}

/// Return the last `max` characters of `text`, aligning the cut to a line
/// boundary so the digest never starts mid-word.
fn tail_chars(text: &str, max: usize) -> (String, bool) {
    if text.chars().count() <= max {
        return (text.to_string(), false);
    }
    let tail: String = text
        .chars()
        .rev()
        .take(max)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let cleaned = match tail.find('\n') {
        Some(pos) if pos < tail.len() - 1 => tail[pos + 1..].to_string(),
        _ => tail,
    };
    (cleaned, true)
}

pub fn run_all(
    repo_root: &Path,
    commands: &[String],
    logger: &Logger,
    kind: ValidationKind,
) -> Result<ValidationReport> {
    let mut results = Vec::new();
    let mut all_pass = true;
    let tag = kind.tag();

    for command in commands {
        logger.info(&format!("[{tag}] $ {command}"));
        let output = shell_command(repo_root, command)
            .with_context(|| format!("failed to execute validation command: {command}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if logger.verbose() {
            if !stdout.is_empty() {
                print!("{stdout}");
                if !stdout.ends_with('\n') {
                    println!();
                }
                logger.append_raw(&stdout);
            }
            if !stderr.is_empty() {
                eprint!("{stderr}");
                if !stderr.ends_with('\n') {
                    eprintln!();
                }
                logger.append_raw(&stderr);
            }
        }
        let passed = output.status.success();
        all_pass &= passed;
        if passed {
            logger.info(&format!("[{tag}] ✓ checks clean"));
        } else {
            logger.info(&format!("[{tag}] issues found; output captured"));
        }
        results.push(ValidationCommandResult {
            command: command.clone(),
            exit_code: output.status.code(),
            stdout,
            stderr,
        });
    }

    Ok(ValidationReport {
        passed: all_pass,
        commands: results,
    })
}

#[cfg(unix)]
fn shell_command(repo_root: &Path, command: &str) -> Result<std::process::Output> {
    Ok(Command::new("sh")
        .current_dir(repo_root)
        .args(["-lc", command])
        .output()?)
}

#[cfg(windows)]
fn shell_command(repo_root: &Path, command: &str) -> Result<std::process::Output> {
    Ok(Command::new("cmd")
        .current_dir(repo_root)
        .args(["/C", command])
        .output()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_validation_set_is_vacuously_passed() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::new(crate::logging::TimestampMode::None, None, false).unwrap();
        let report = run_all(dir.path(), &[], &logger, ValidationKind::Inspection).unwrap();
        assert!(report.passed);
    }

    #[test]
    fn validation_kind_uses_non_terminal_inspection_label() {
        assert_eq!(ValidationKind::Inspection.tag(), "inspect");
        assert_eq!(ValidationKind::Final.tag(), "validate");
    }

    #[test]
    fn non_verbose_inspection_keeps_raw_failure_word_out_of_progress_log() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("wheel.log");
        let logger =
            Logger::new(crate::logging::TimestampMode::None, Some(&log_path), false).unwrap();
        std::fs::write(
            dir.path().join("validator-output.txt"),
            "RAW_VALIDATOR_FAIL_MARKER",
        )
        .unwrap();
        #[cfg(unix)]
        let command = "cat validator-output.txt; exit 1".to_string();
        #[cfg(windows)]
        let command = "type validator-output.txt & exit /b 1".to_string();

        let report = run_all(dir.path(), &[command], &logger, ValidationKind::Inspection).unwrap();

        assert!(!report.passed);
        assert!(report.commands[0]
            .stdout
            .contains("RAW_VALIDATOR_FAIL_MARKER"));
        let progress_log = std::fs::read_to_string(log_path).unwrap();
        assert!(progress_log.contains("[inspect] issues found; output captured"));
        assert!(!progress_log.contains("RAW_VALIDATOR_FAIL_MARKER"));
    }

    #[test]
    fn evidence_digest_truncates_large_streams() {
        let report = ValidationReport {
            passed: true,
            commands: vec![ValidationCommandResult {
                command: "cargo test".into(),
                exit_code: Some(0),
                stdout: format!("line\n{}", "x".repeat(100_000)),
                stderr: String::new(),
            }],
        };
        let digest = report.evidence_digest(2000);
        assert!(digest.chars().count() < 2500);
        assert!(digest.contains("(truncated, tail only)"));
        assert!(digest.contains("-> PASS"));
    }

    #[test]
    fn tail_chars_aligns_to_line_boundary() {
        let text = "first line\nsecond line\nthird line\n";
        let (tail, truncated) = tail_chars(text, 15);
        assert!(truncated);
        assert!(!tail.starts_with("st line"));
        assert!(tail.starts_with("second") || tail.starts_with("third"));
    }
}
