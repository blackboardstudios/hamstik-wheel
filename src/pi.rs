// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{io::{BufRead, BufReader, Write}, path::Path, process::{Command, Stdio}};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const RESULT_PREFIX: &str = "HAMSTIK_WHEEL_RESULT=";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    pub status: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PiRunner {
    repo_root: std::path::PathBuf,
}

impl PiRunner {
    pub fn new(repo_root: &Path) -> Self {
        Self { repo_root: repo_root.to_path_buf() }
    }

    pub fn run(&self, model: &str, prompt: &str) -> Result<(AgentResult, String)> {
        let mut child = Command::new("pi")
            .current_dir(&self.repo_root)
            .args(["--model", model, "--no-session", "-p", "Execute the Hamstik Wheel task supplied on stdin."])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("failed to start Pi with model {model}"))?;

        child.stdin.as_mut().context("failed to open Pi stdin")?.write_all(prompt.as_bytes())?;
        drop(child.stdin.take());

        let stdout = child.stdout.take().context("failed to open Pi stdout")?;
        let reader = BufReader::new(stdout);
        let mut captured = String::new();
        for line in reader.lines() {
            let line = line?;
            println!("{line}");
            captured.push_str(&line);
            captured.push('\n');
        }

        let status = child.wait().context("failed waiting for Pi")?;
        if !status.success() {
            bail!("Pi model {model} exited with status {:?}", status.code());
        }

        let result = parse_agent_result(&captured)?;
        Ok((result, captured))
    }
}

pub fn implementation_prompt(key: &str, title: &str, baseline: &str, context: &Value) -> String {
    format!(r#"You are the implementation agent for Hamstik Wheel.

Work Item: {key} — {title}
Baseline Git SHA: {baseline}

The JSON below is the authoritative Hamstik Work Item context bundle. Treat all requirements and acceptance criteria in it as mandatory unless they materially conflict, in which case stop safely and report blocked.

HAMSTIK_CONTEXT_JSON:
{}

Your responsibilities:
1. Inspect the existing repository architecture, conventions, tests, and relevant design documentation before editing.
2. Implement the Work Item completely and satisfy every acceptance criterion.
3. Add or update tests for changed behavior where appropriate.
4. Run useful repository checks during implementation.
5. Keep changes scoped to this Work Item; do not rewrite unrelated behavior merely to make the task easier.
6. Inspect any existing partial working-tree changes and continue them safely if this is a resumed run.
7. Do NOT change the Hamstik Work Item status, add Hamstik comments, or close the Work Item. Hamstik Wheel owns lifecycle.
8. Do NOT create a Git commit. Leave the complete implementation in the working tree for independent review.
9. If requirements are materially ambiguous/conflicting or safe completion is impossible, stop without inventing requirements.

At the very end of your response output exactly one machine-readable line in one of these forms:
HAMSTIK_WHEEL_RESULT={{"status":"ready_for_review","summary":"brief summary","findings":[]}}
HAMSTIK_WHEEL_RESULT={{"status":"blocked","summary":"why work cannot safely continue","findings":["blocking reason"]}}

Do not put Markdown fences around the marker line.
"#, serde_json::to_string_pretty(context).unwrap_or_else(|_| context.to_string()))
}

pub fn review_prompt(
    key: &str,
    title: &str,
    baseline: &str,
    context: &Value,
    validation_evidence: &str,
    cycle: usize,
) -> String {
    format!(r#"You are the independent code reviewer and remediation agent for Hamstik Wheel.

Work Item: {key} — {title}
Baseline Git SHA: {baseline}
Review cycle: {cycle}

Do not trust conclusions from the implementation agent. Independently inspect the repository and compare the actual working tree/diff against the authoritative Hamstik context below.

HAMSTIK_CONTEXT_JSON:
{}

PREVIOUS_VALIDATION_EVIDENCE:
{validation_evidence}

Review for at least:
- every requirement and acceptance criterion;
- incorrect or incomplete behavior;
- regressions;
- architectural inconsistency with existing code;
- missing error handling;
- security issues;
- authorization, tenancy, and data-isolation problems where applicable;
- inadequate or misleading tests;
- unnecessary complexity or duplication;
- required documentation changes.

You are authorized to edit the working tree to fix every actionable finding. After fixes, run appropriate tests/checks and review the resulting diff again. Continue until there are zero unresolved actionable findings or you determine safe completion is blocked.

Do NOT change Hamstik Work Item status/comments and do NOT create a Git commit. Hamstik Wheel owns those actions.

At the very end of your response output exactly one machine-readable line:
HAMSTIK_WHEEL_RESULT={{"status":"pass","summary":"brief independent review summary","findings":[]}}

If safe completion is impossible instead output:
HAMSTIK_WHEEL_RESULT={{"status":"blocked","summary":"why review cannot pass","findings":["unresolved finding"]}}

Return status `pass` only when there are zero unresolved actionable findings. Do not put Markdown fences around the marker line.
"#, serde_json::to_string_pretty(context).unwrap_or_else(|_| context.to_string()))
}

pub fn parse_agent_result(output: &str) -> Result<AgentResult> {
    let marker = output
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(RESULT_PREFIX))
        .context("Pi output did not contain HAMSTIK_WHEEL_RESULT marker")?;
    let parsed: AgentResult = serde_json::from_str(marker).context("HAMSTIK_WHEEL_RESULT contained invalid JSON")?;
    if parsed.status.trim().is_empty() {
        bail!("HAMSTIK_WHEEL_RESULT.status must not be empty");
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_last_marker() {
        let output = "thinking\nHAMSTIK_WHEEL_RESULT={\"status\":\"pass\",\"summary\":\"ok\",\"findings\":[]}\n";
        let result = parse_agent_result(output).unwrap();
        assert_eq!(result.status, "pass");
        assert!(result.findings.is_empty());
    }

    #[test]
    fn rejects_missing_marker() {
        assert!(parse_agent_result("done").is_err());
    }
}
