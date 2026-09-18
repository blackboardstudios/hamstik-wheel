// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{activity::ActivityTracker, logging::Logger};

const RESULT_PREFIX: &str = "HAMSTIK_WHEEL_RESULT=";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    pub status: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<String>,
}

/// Outcome of one Pi invocation. The transcript is always captured so callers can
/// persist it even when the agent failed to emit a parsable result marker.
#[derive(Debug)]
pub struct PiRun {
    pub result: Result<AgentResult>,
    pub transcript: String,
}

#[derive(Debug, Clone)]
pub struct PiRunner {
    repo_root: std::path::PathBuf,
}

impl PiRunner {
    pub fn new(repo_root: &Path) -> Self {
        Self {
            repo_root: repo_root.to_path_buf(),
        }
    }

    pub fn run(&self, model: &str, prompt: &str, logger: &Logger) -> Result<PiRun> {
        let mut child = Command::new("pi")
            .current_dir(&self.repo_root)
            .args([
                "--model",
                model,
                "--no-session",
                "--mode",
                "json",
                "-p",
                "Execute the Hamstik Wheel task supplied on stdin.",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("failed to start Pi with model {model}"))?;

        let write_error = match child
            .stdin
            .as_mut()
            .context("failed to open Pi stdin")?
            .write_all(prompt.as_bytes())
        {
            Ok(()) => None,
            Err(error) => Some(anyhow!("failed writing prompt to Pi stdin: {error}")),
        };
        drop(child.stdin.take());

        let stdout = child.stdout.take().context("failed to open Pi stdout")?;
        let reader = BufReader::new(stdout);
        let mut transcript = String::new();
        let mut read_error = None;
        let mut tracker = ActivityTracker::start(logger.activity_enabled());
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if let Some(update) = crate::activity::description_from_event(&line) {
                        tracker.update(update);
                    }
                    transcript.push_str(&line);
                    transcript.push('\n');
                }
                Err(error) => {
                    read_error = Some(anyhow!("failed reading Pi stdout: {error}"));
                    break;
                }
            }
        }
        tracker.finish();

        let status = child.wait().context("failed waiting for Pi")?;
        let result = if let Some(error) = write_error.or(read_error) {
            Err(error)
        } else if !status.success() {
            Err(anyhow!(
                "Pi model {model} exited with status {:?}",
                status.code()
            ))
        } else {
            parse_agent_result(&transcript)
        };

        Ok(PiRun { result, transcript })
    }
}

pub fn implementation_prompt(key: &str, title: &str, baseline: &str, context: &Value) -> String {
    format!(
        r#"You are the implementation agent for Hamstik Wheel.

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
"#,
        serde_json::to_string_pretty(context).unwrap_or_else(|_| context.to_string())
    )
}

pub fn review_prompt(
    key: &str,
    title: &str,
    baseline: &str,
    context: &Value,
    validation_evidence: &str,
    cycle: usize,
) -> String {
    format!(
        r#"You are the independent code reviewer and remediation agent for Hamstik Wheel.

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
"#,
        serde_json::to_string_pretty(context).unwrap_or_else(|_| context.to_string())
    )
}

pub fn parse_agent_result(output: &str) -> Result<AgentResult> {
    // The transcript is the NDJSON event stream from `pi --mode json`. The
    // marker is emitted inside the final assistant text, so it arrives
    // JSON-escaped in the last event; search every line for any occurrence.
    let marker = find_marker_line(output)
        .with_context(|| "Pi output did not contain HAMSTIK_WHEEL_RESULT marker")?;
    let parsed: AgentResult =
        serde_json::from_str(&marker).context("HAMSTIK_WHEEL_RESULT contained invalid JSON")?;
    if parsed.status.trim().is_empty() {
        bail!("HAMSTIK_WHEEL_RESULT.status must not be empty");
    }
    Ok(parsed)
}

/// Find the marker payload anywhere in the transcript. Handles both a plain
/// line occurrence and a JSON-escaped occurrence inside an NDJSON event line
/// by parsing the line and searching every decoded string value, which
/// sidesteps escape-state tracking entirely.
fn find_marker_line(output: &str) -> Result<String> {
    for line in output.lines().rev() {
        let Some(start) = line.find(RESULT_PREFIX) else {
            continue;
        };
        let rest = &line[start + RESULT_PREFIX.len()..];
        let trimmed = rest.trim();
        // Plain (unescaped) occurrence: the marker JSON directly on the line.
        if trimmed.starts_with('{') && serde_json::from_str::<Value>(trimmed).is_ok() {
            return Ok(trimmed.to_string());
        }
        // Escaped occurrence inside an event line: decode the line as JSON
        // and search its string values for the marker payload.
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            let mut found = None;
            search_strings(&value, &mut |text| {
                if found.is_none() {
                    if let Some(payload) = marker_payload_in(text) {
                        found = Some(payload);
                    }
                }
            });
            if let Some(payload) = found {
                return Ok(payload);
            }
        }
        // Fallback heuristic for lines that are not valid JSON.
        if let Some(escaped) = extract_balanced_object(rest) {
            return Ok(unescape_json(&escaped));
        }
    }
    bail!("no marker found")
}

fn search_strings(value: &Value, visit: &mut dyn FnMut(&str)) {
    match value {
        Value::String(text) => visit(text),
        Value::Array(items) => items.iter().for_each(|item| search_strings(item, visit)),
        Value::Object(map) => map.values().for_each(|item| search_strings(item, visit)),
        _ => {}
    }
}

/// Extract the balanced `{...}` payload that starts at/after RESULT_PREFIX
/// inside an already-decoded string value.
fn marker_payload_in(text: &str) -> Option<String> {
    let position = text.find(RESULT_PREFIX)? + RESULT_PREFIX.len();
    let rest = &text[position..];
    let start = rest.find('{')?;
    let bytes = rest.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut index = start;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\\' {
            index += 2;
            continue;
        }
        match byte {
            b'"' => in_string = !in_string,
            b'{' if !in_string => depth += 1,
            b'}' if !in_string => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(rest[start..=index].to_string());
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// Extract the `{...}` JSON object starting at the first `{` in `rest`,
/// tracking JSON string state (`\x` escapes skip two bytes) so braces inside
/// string values do not disturb balance.
fn extract_balanced_object(rest: &str) -> Option<String> {
    let start = rest.find('{')?;
    let bytes = rest.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut index = start;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\\' {
            index += 2;
            continue;
        }
        match byte {
            b'"' => in_string = !in_string,
            b'{' if !in_string => depth += 1,
            b'}' if !in_string => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(rest[start..=index].to_string());
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn unescape_json(escaped: &str) -> String {
    serde_json::from_str::<Value>(&format!("\"{escaped}\""))
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| escaped.to_string())
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

    #[test]
    fn parses_marker_prefixed_by_bullet() {
        let output = "summary\n- HAMSTIK_WHEEL_RESULT={\"status\":\"ready_for_review\",\"summary\":\"done\",\"findings\":[]}\n";
        let result = parse_agent_result(output).unwrap();
        assert_eq!(result.status, "ready_for_review");
    }

    #[test]
    fn rejects_malformed_marker_json() {
        let output = "HAMSTIK_WHEEL_RESULT={not json}\n";
        assert!(parse_agent_result(output).is_err());
    }

    #[test]
    fn parses_marker_from_json_event_line() {
        // Simulate the NDJSON agent_end event with the marker inside message text.
        let event = r#"{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"text","text":"All done.\nHAMSTIK_WHEEL_RESULT={\"status\":\"ready_for_review\",\"summary\":\"ok\",\"findings\":[]}"}]}]}"#;
        let result = parse_agent_result(event).unwrap();
        assert_eq!(result.status, "ready_for_review");
        assert_eq!(result.summary, "ok");
    }

    #[test]
    fn parses_marker_from_last_turn_end_event() {
        let first = r#"{"type":"turn_end","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}"#;
        let last = r#"{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"text","text":"done\nHAMSTIK_WHEEL_RESULT={\"status\":\"pass\",\"summary\":\"reviewed\",\"findings\":[]}"}]}]}"#;
        let output = format!("{first}\n{last}\n");
        let result = parse_agent_result(&output).unwrap();
        assert_eq!(result.status, "pass");
        assert_eq!(result.summary, "reviewed");
    }

    #[test]
    fn parses_marker_with_nested_braces_in_strings() {
        let event = r#"{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"text","text":"HAMSTIK_WHEEL_RESULT={\"status\":\"blocked\",\"summary\":\"json has }{ braces \\\" quotes\",\"findings\":[\"a\"]}"}]}]}"#;
        let result = parse_agent_result(event).unwrap();
        assert_eq!(result.status, "blocked");
        assert_eq!(result.findings, vec!["a".to_string()]);
        assert!(result.summary.contains("braces"));
    }
}
