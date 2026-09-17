// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{path::Path, process::Command};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
    pub fn evidence(&self) -> String {
        let mut out = format!("overall: {}\n", if self.passed { "PASS" } else { "FAIL" });
        for item in &self.commands {
            out.push_str(&format!("\n$ {}\nexit: {:?}\n", item.command, item.exit_code));
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
}

pub fn run_all(repo_root: &Path, commands: &[String]) -> Result<ValidationReport> {
    let mut results = Vec::new();
    let mut all_pass = true;

    for command in commands {
        println!("[validate] $ {command}");
        let output = shell_command(repo_root, command)
            .with_context(|| format!("failed to execute validation command: {command}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !stdout.is_empty() { print!("{stdout}"); }
        if !stderr.is_empty() { eprint!("{stderr}"); }
        let passed = output.status.success();
        all_pass &= passed;
        results.push(ValidationCommandResult {
            command: command.clone(),
            exit_code: output.status.code(),
            stdout,
            stderr,
        });
    }

    Ok(ValidationReport { passed: all_pass, commands: results })
}

#[cfg(unix)]
fn shell_command(repo_root: &Path, command: &str) -> Result<std::process::Output> {
    Ok(Command::new("sh").current_dir(repo_root).args(["-lc", command]).output()?)
}

#[cfg(windows)]
fn shell_command(repo_root: &Path, command: &str) -> Result<std::process::Output> {
    Ok(Command::new("cmd").current_dir(repo_root).args(["/C", command]).output()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_validation_set_is_vacuously_passed() {
        let dir = tempfile::tempdir().unwrap();
        let report = run_all(dir.path(), &[]).unwrap();
        assert!(report.passed);
    }
}
