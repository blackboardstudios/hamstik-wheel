// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{path::Path, process::Command};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::logging::Logger;

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
            "overall: {}. Full repository validation has ALREADY been run by Hamstik Wheel — do not re-run it; your job is diff review, not re-verification.\n(full untruncated output is saved in .git/hamstik-wheel/logs/<KEY>/pre-review-validation.log)\n",
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

pub fn run_all(repo_root: &Path, commands: &[String], logger: &Logger) -> Result<ValidationReport> {
    let mut results = Vec::new();
    let mut all_pass = true;

    for command in commands {
        logger.info(&format!("[validate] $ {command}"));
        let output = shell_command(repo_root, command)
            .with_context(|| format!("failed to execute validation command: {command}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !stdout.is_empty() {
            print!("{stdout}");
            logger.append_raw(&stdout);
        }
        if !stderr.is_empty() {
            eprint!("{stderr}");
            logger.append_raw(&stderr);
        }
        let passed = output.status.success();
        all_pass &= passed;
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
        let logger = Logger::new(crate::logging::TimestampMode::None, None).unwrap();
        let report = run_all(dir.path(), &[], &logger).unwrap();
        assert!(report.passed);
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
