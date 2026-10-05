// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    activity::ActivityTracker,
    logging::Logger,
    process::{self, ManagedChild},
    security::{self, Redactor},
};

const RESULT_PREFIX: &str = "HAMSTIK_WHEEL_RESULT=";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    pub status: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<String>,
    /// Required checks the agent could not verify. A successful marker must
    /// never hide these behind a prose-only caveat.
    #[serde(default)]
    pub unverified_checks: Vec<String>,
    /// Required checks that Wheel is configured to run. A declaration is not
    /// evidence: the engine validates the exact command and runs it at both
    /// validation gates.
    #[serde(default)]
    pub pending_scheduled_checks: Vec<PendingScheduledCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingScheduledCheck {
    pub command: String,
}

#[derive(Debug)]
pub struct ProviderFailure {
    pub configured_model: String,
    pub resolved_model: String,
    pub provider: String,
    pub message: String,
    pub rate_limited: bool,
    pub retryable: bool,
}

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Pi model {} provider {} {} (resolved model {}): {}",
            self.configured_model,
            self.provider,
            if self.rate_limited {
                "rate-limited"
            } else {
                "request failed"
            },
            self.resolved_model,
            self.message
        )
    }
}

impl std::error::Error for ProviderFailure {}

/// Inspect terminal assistant events, not prompt/tool text. A successful later
/// response supersedes errors from Pi's internal retry loop.
pub(crate) fn provider_failure(output: &str, configured_model: &str) -> Option<ProviderFailure> {
    for line in output.lines().rev() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) != Some("message_end") {
            continue;
        }
        let Some(message) = event.get("message") else {
            continue;
        };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if message.get("stopReason").and_then(Value::as_str) != Some("error") {
            return None;
        }
        let detail = message
            .get("errorMessage")
            .and_then(Value::as_str)
            .unwrap_or("provider returned an unspecified error");
        let lower = detail.to_ascii_lowercase();
        let rate_limited =
            lower.contains("429") || lower.contains("rate-limit") || lower.contains("rate limit");
        return Some(ProviderFailure {
            configured_model: configured_model.to_string(),
            resolved_model: message
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(configured_model)
                .to_string(),
            provider: message
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            message: detail.to_string(),
            rate_limited,
            retryable: rate_limited
                || [
                    "500",
                    "502",
                    "503",
                    "504",
                    "overloaded",
                    "timeout",
                    "connection",
                ]
                .iter()
                .any(|needle| lower.contains(needle)),
        });
    }
    None
}

/// Outcome of one Pi invocation. The transcript is always captured so callers can
/// persist it even when the agent failed to emit a parsable result marker.
#[derive(Debug)]
pub struct PiRun {
    pub result: Result<AgentResult>,
    pub transcript: String,
}

#[derive(Debug)]
pub struct PiRunner {
    repo_root: PathBuf,
    runtime: PathBuf,
    sandbox_config: String,
}

impl PiRunner {
    pub fn new(repo_root: &Path, runtime: PathBuf, read_only_paths: &[String]) -> Result<Self> {
        let runtime = runtime.join(format!(
            "{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let sandbox_config = security::prepare_runtime(repo_root, &runtime, read_only_paths)?;
        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            runtime,
            sandbox_config,
        })
    }

    pub fn sandbox(&self) -> (&Path, &str) {
        (&self.runtime, &self.sandbox_config)
    }

    pub fn preflight(&self) -> Result<()> {
        security::check_runtime(&self.runtime, &self.sandbox_config)?;
        let mut command = Command::new("pi");
        command
            .current_dir(&self.repo_root)
            .args([
                "--no-session",
                "--mode",
                "json",
                "--no-extensions",
                "--no-skills",
                "--no-prompt-templates",
                "--no-themes",
                "--no-builtin-tools",
                "--tools",
                security::TOOLS,
                "--extension",
            ])
            .arg(self.runtime.join("guard.mjs"))
            .arg("--wheel-guard-check")
            .env("HAMSTIK_WHEEL_SANDBOX", &self.sandbox_config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        process::configure(&mut command);
        let mut child = ManagedChild(command.spawn().context("failed to verify Pi tool guard")?);
        let stdout = child.0.stdout.take().unwrap();
        let stderr = child.0.stderr.take().unwrap();
        let reader = std::thread::spawn(move || {
            use std::io::Read;
            let mut s = String::new();
            let _ = BufReader::new(stdout).read_to_string(&mut s);
            s
        });
        let errors = std::thread::spawn(move || {
            use std::io::Read;
            let mut s = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut s);
            s
        });
        let status = child.wait(Some(Duration::from_secs(20)));
        let output = reader.join().unwrap_or_default();
        let errors = errors.join().unwrap_or_default();
        if !status?.success()
            || !output.lines().any(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .is_some_and(|v| v["type"] == "wheel_guard_ready" && v["version"] == 1)
            })
        {
            bail!("Wheel tool guard preflight failed; update Pi to a version supporting custom tools and explicit extensions: {}", Redactor::new(&self.repo_root).text(&errors));
        }
        Ok(())
    }

    pub fn run(
        &self,
        model: &str,
        prompt: &str,
        timeout: Option<Duration>,
        logger: &Logger,
        log_path: &Path,
    ) -> Result<PiRun> {
        let redactor = Redactor::new(&self.repo_root);
        let mut log = std::fs::OpenOptions::new().append(true).open(log_path)?;
        let mut command = Command::new("pi");
        command.current_dir(&self.repo_root)
            .args(["--model", model, "--no-session", "--mode", "json",
                "--no-extensions", "--no-skills", "--no-prompt-templates", "--no-themes",
                "--no-builtin-tools", "--tools", security::TOOLS, "--extension"])
            .arg(self.runtime.join("guard.mjs"))
            .env("HAMSTIK_WHEEL_SANDBOX", &self.sandbox_config)
            .args(["-p", "Execute the Hamstik Wheel task supplied on stdin. Use wheel_result as your final tool action."])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        process::configure(&mut command);
        let mut child = ManagedChild(
            command
                .spawn()
                .with_context(|| format!("failed to start Pi with model {model}"))?,
        );
        let mut stdin = child.0.stdin.take().context("failed to open Pi stdin")?;
        let prompt = format!("{}\nRepository root: {}. All file and shell operations must use the wheel_* tools. Shell tools have no host network, credentials, Docker sockets, or writable host paths outside this repository. Do not try to bypass isolation. Required database/browser checks belong in Wheel's configured validation commands. Submit wheel_result with all unverified required checks as your final tool action; prose alone is not a verdict.\n", prompt, self.repo_root.display());
        // Drain output and write input concurrently so pipe pressure cannot
        // prevent the wall-clock deadline from being enforced.
        let writer = std::thread::spawn(move || stdin.write_all(prompt.as_bytes()));
        let stdout = child.0.stdout.take().context("failed to open Pi stdout")?;
        let stderr = child.0.stderr.take().context("failed to open Pi stderr")?;
        let error_redactor = redactor.clone();
        let mut error_log = log.try_clone()?;
        let error_reader = std::thread::spawn(move || -> Result<()> {
            for line in BufReader::new(stderr).lines() {
                let line = error_redactor.text(&line?);
                let event = serde_json::json!({"type":"wheel_stderr", "text":line});
                error_log.write_all(format!("{event}\n").as_bytes())?;
                error_log.flush()?;
            }
            Ok(())
        });
        let tracker = ActivityTracker::start(logger.activity_enabled());
        let activity = tracker.handle();
        let reader = std::thread::spawn(move || -> Result<String> {
            let mut transcript = String::new();
            for line in BufReader::new(stdout).lines() {
                if let Some(line) = redactor.event(&line?) {
                    if let Some(update) = crate::activity::description_from_event(&line) {
                        activity.update(update);
                    }
                    // Append complete redacted events as they arrive, before
                    // parsing the result. Interrupted sessions remain inspectable.
                    log.write_all(format!("{line}\n").as_bytes())?;
                    log.flush()?;
                    transcript.push_str(&line);
                    transcript.push('\n');
                }
            }
            Ok(transcript)
        });
        let status = child.wait(timeout);
        let write_result = writer
            .join()
            .map_err(|_| anyhow!("Pi stdin writer panicked"))?;
        let transcript = reader
            .join()
            .map_err(|_| anyhow!("Pi output reader panicked"))??;
        error_reader
            .join()
            .map_err(|_| anyhow!("Pi stderr reader panicked"))??;
        tracker.finish();
        let result = match status {
            Err(error) => Err(error.context(format!(
                "Pi model {model} exceeded the session timeout or failed"
            ))),
            Ok(status) if !status.success() => Err(anyhow!(
                "Pi model {model} exited with status {:?}",
                status.code()
            )),
            Ok(_) => {
                if let Some(failure) = provider_failure(&transcript, model) {
                    Err(failure.into())
                } else if write_result.is_err() {
                    Err(anyhow!("failed writing prompt to Pi stdin"))
                } else if !transcript.lines().any(|line| {
                    serde_json::from_str::<Value>(line)
                        .ok()
                        .is_some_and(|e| e["type"] == "wheel_guard_ready" && e["version"] == 1)
                }) {
                    Err(anyhow!(
                        "Wheel tool guard did not initialize; refusing an unguarded session"
                    ))
                } else if !transcript.lines().any(|line| {
                    serde_json::from_str::<Value>(line)
                        .ok()
                        .is_some_and(|e| e["type"] == "wheel_result")
                }) {
                    Err(anyhow!("Pi output did not contain HAMSTIK_WHEEL_RESULT/wheel_result; submit the wheel_result tool"))
                } else {
                    parse_agent_result(&transcript)
                        .with_context(|| format!("Pi model {model} returned invalid result output"))
                }
            }
        };
        Ok(PiRun { result, transcript })
    }
}

impl Drop for PiRunner {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.runtime);
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
4. Run useful repository checks during implementation. Your session has a hard wall-clock limit: prefer targeted checks (`cargo check -p <crate>`) over the full precheck suite — Hamstik Wheel runs its configured validation commands after implementation.
5. Keep changes scoped to this Work Item; do not rewrite unrelated behavior merely to make the task easier.
6. Inspect any existing partial working-tree changes and continue them safely if this is a resumed run. If a preserved-work commit is supplied below, inspect that exact commit read-only and copy what is salvageable. NEVER merge, checkout, rebase, or otherwise move branch refs — those are Hamstik Wheel's to manage.
7. Do NOT change the Hamstik Work Item status, add Hamstik comments, or close the Work Item. Hamstik Wheel owns lifecycle.
8. Do NOT create a Git commit. Leave the complete implementation in the working tree for independent review.
9. If requirements are materially ambiguous/conflicting or safe completion is impossible, stop without inventing requirements.
10. Identify validation required by repository instructions and the changed behavior (including populated database migrations and integration tests). Do not assume Wheel's generic prechecks include these. If a required check cannot run but its exact command is listed in WHEEL_VALIDATION_CONFIG and applies to the actual changed paths, declare it as `pending_scheduled_checks: [{{"command":"..."}}]`; an exact configured wrapper command is allowed when it invokes the required check(s). Wheel will run it before review and again at the final gate. All other required checks must return blocked and appear in unverified_checks. Never hide unrun required checks in prose while returning ready_for_review.

Your final tool action MUST call wheel_result. Supply status ready_for_review or blocked, a concrete summary, findings, unverified_checks, and pending_scheduled_checks (all arrays are required even when empty). Do not merely print a marker or verdict in prose. No editing or checks are possible after you submit the result.
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

PREVIOUS_VALIDATION_EVIDENCE (redacted digest; private Wheel metadata is unavailable to agent tools):
{validation_evidence}

Budget your work: you have a hard session wall-clock limit. Inspect the diff (`git diff {baseline}`), read the files it touches and relevant dependencies, and do not dump large outputs to the console.

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

You are authorized to edit the working tree to fix every actionable finding. After fixes, review the resulting diff again. Continue until there are zero unresolved actionable findings or you determine safe completion is blocked.

Work within the session wall-clock limit:
- Inspect `git diff {baseline}` first; read touched files and relevant dependencies.
- Do NOT run full-repository verification: never run precheck scripts, `cargo fmt --all`, `cargo clippy --workspace`, `cargo build --workspace --release`, or the whole test suite. Hamstik Wheel runs the configured validation commands after implementation and again after your review. Do not treat a passing configured suite as evidence for checks it does not include. A check explicitly declared in pending_scheduled_checks is scheduled evidence, not an unverified check; every other required check absent from supplied evidence must return blocked with unverified_checks. Never return pass with required validation missing.
- Prefer targeted checks: `cargo check -p <touched-crate> --all-targets` or `cargo test -p <touched-crate> --test <relevant-test>`.
- Prefer small, decisive fixes over exploratory loops. If time expires before all requirements can be verified, submit a blocked wheel_result with the unresolved findings.

Do NOT change Hamstik Work Item status/comments and do NOT create a Git commit. Hamstik Wheel owns those actions.

Your final tool action MUST call wheel_result with status pass or blocked, a concrete independent-review summary, findings, unverified_checks, and pending_scheduled_checks. All arrays must be present. Return pass only with zero unresolved findings and zero unverified required checks. Do not declare arbitrary commands pending: only exact commands in the supplied validation configuration that apply to changed paths are allowed, including configured wrapper commands that invoke the required checks. Do not merely print a marker or verdict in prose. No editing or checks are possible after submission.
"#,
        serde_json::to_string_pretty(context).unwrap_or_else(|_| context.to_string())
    )
}

pub fn parse_agent_result(output: &str) -> Result<AgentResult> {
    for line in output.lines().rev() {
        if let Ok(event) = serde_json::from_str::<Value>(line) {
            if event["type"] == "wheel_result" {
                if !event["result"]["findings"].is_array()
                    || !event["result"]["unverified_checks"].is_array()
                    || !event["result"]["summary"].is_string()
                {
                    bail!("wheel_result must explicitly declare findings, summary, and unverified_checks");
                }
                let result: AgentResult = serde_json::from_value(event["result"].clone())
                    .context("wheel_result contained invalid result")?;
                if !["ready_for_review", "pass", "blocked"].contains(&result.status.as_str()) {
                    bail!("invalid wheel_result status");
                }
                return Ok(result);
            }
        }
    }
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

/// Find the marker payload in the transcript. Handles both a plain line
/// occurrence and a JSON-escaped occurrence inside an NDJSON event line by
/// parsing the line and searching assistant message text, which sidesteps
/// escape-state tracking entirely.
///
/// Only assistant message content is searched. Prompt text (system/user
/// roles) carries example markers that must never be accepted as the
/// agent's verdict (observed: the prompt examples embedded in an
/// `agent_end` event were matched instead of a reviewer's real `blocked`
/// verdict, letting an unimplemented item reach the commit phase).
fn find_marker_line(output: &str) -> Result<String> {
    for line in output.lines().rev() {
        let Some(start) = line.find(RESULT_PREFIX) else {
            continue;
        };
        let rest = &line[start + RESULT_PREFIX.len()..];

        // Plain (unescaped) occurrence: the marker JSON directly on the line.
        let trimmed = rest.trim();
        if trimmed.starts_with('{') && serde_json::from_str::<Value>(trimmed).is_ok() {
            if is_placeholder_marker(trimmed) {
                continue;
            }
            return Ok(trimmed.to_string());
        }

        // Escaped occurrence inside an event line: decode the line as JSON
        // and search assistant message text values for the marker payload.
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            let mut texts = Vec::new();
            collect_assistant_texts(&value, &mut texts);
            for text in texts.iter().rev() {
                if let Some(payload) = last_marker_payload_in(text) {
                    if !is_placeholder_marker(&payload) {
                        return Ok(payload);
                    }
                }
            }
            continue;
        }

        // Fallback heuristic for lines that are neither plain markers nor
        // valid JSON events.
        if let Some(escaped) = extract_balanced_object(rest) {
            let payload = unescape_json(&escaped);
            if !is_placeholder_marker(&payload) {
                return Ok(payload);
            }
        }
    }
    bail!("no marker found")
}

/// The prompts embed example marker payloads that the agent must replace
/// with its own verdict. A payload whose summary is verbatim placeholder
/// text carries no verdict information; treat it as absent.
fn is_placeholder_marker(payload: &str) -> bool {
    const PLACEHOLDER_SUMMARIES: [&str; 2] = ["brief summary", "brief independent review summary"];
    serde_json::from_str::<AgentResult>(payload)
        .map(|result| PLACEHOLDER_SUMMARIES.contains(&result.summary.as_str()))
        .unwrap_or(false)
}

/// Collect the text of every assistant message in a decoded transcript
/// event. Recursion into the decoded structure covers both the
/// `message_end` shape (`{"message": {...}}`) and the `agent_end` shape
/// (`{"messages": [...]}`); system/user messages are skipped because only
/// the assistant emits the result marker.
fn collect_assistant_texts(value: &Value, texts: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if map.get("role").and_then(Value::as_str) == Some("assistant") {
                if let Some(content) = map.get("content").and_then(Value::as_array) {
                    for item in content {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            texts.push(text.to_string());
                        }
                    }
                }
            }
            for child in map.values() {
                collect_assistant_texts(child, texts);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_assistant_texts(item, texts);
            }
        }
        _ => {}
    }
}

/// Extract the balanced `{...}` payload that starts at/after the LAST
/// `RESULT_PREFIX` occurrence inside an already-decoded string value, so a
/// quoted prompt example preceding the real verdict does not shadow it.
fn last_marker_payload_in(text: &str) -> Option<String> {
    let position = text.rfind(RESULT_PREFIX)? + RESULT_PREFIX.len();
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
    fn provider_errors_preserve_configured_and_resolved_model() {
        let output = serde_json::json!({"type":"message_end", "message": {
            "role":"assistant", "stopReason":"error", "provider":"gateway",
            "model":"actual-model", "errorMessage":"429: upstream rate-limited"
        }})
        .to_string();
        let failure = provider_failure(&output, "gateway/pattern").unwrap();
        assert!(failure.rate_limited && failure.retryable);
        assert_eq!(failure.configured_model, "gateway/pattern");
        assert_eq!(failure.resolved_model, "actual-model");
        assert!(failure.to_string().contains("429: upstream rate-limited"));
    }

    #[test]
    fn successful_internal_retry_supersedes_provider_error() {
        let error = serde_json::json!({"type":"message_end", "message": {
            "role":"assistant", "stopReason":"error", "errorMessage":"429"
        }});
        let success = serde_json::json!({"type":"message_end", "message": {
            "role":"assistant", "stopReason":"stop", "content":[{"type":"text", "text":"done"}]
        }});
        assert!(provider_failure(&format!("{error}\n{success}\n"), "m").is_none());
        let tool = serde_json::json!({"type":"message_end", "message": {
            "role":"toolResult", "stopReason":"error", "errorMessage":"429"
        }});
        assert!(provider_failure(&format!("{success}\n{tool}\n"), "m").is_none());
    }

    #[test]
    fn authentication_errors_are_not_retried() {
        let output = serde_json::json!({"type":"message_end", "message": {
            "role":"assistant", "stopReason":"error", "errorMessage":"401: invalid API key"
        }})
        .to_string();
        let failure = provider_failure(&output, "m").unwrap();
        assert!(!failure.retryable);
        assert!(!failure.rate_limited);
    }

    #[test]
    fn parses_last_marker() {
        let output = "thinking\nHAMSTIK_WHEEL_RESULT={\"status\":\"pass\",\"summary\":\"ok\",\"findings\":[]}\n";
        let result = parse_agent_result(output).unwrap();
        assert_eq!(result.status, "pass");
        assert!(result.findings.is_empty());
    }

    #[test]
    fn parses_structured_pending_scheduled_checks() {
        let output = r#"HAMSTIK_WHEEL_RESULT={"status":"ready_for_review","summary":"implemented","findings":[],"unverified_checks":[],"pending_scheduled_checks":[{"command":"pnpm migrations:test"}]}"#;
        let result = parse_agent_result(output).unwrap();
        assert_eq!(result.pending_scheduled_checks.len(), 1);
        assert_eq!(
            result.pending_scheduled_checks[0].command,
            "pnpm migrations:test"
        );
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

    #[test]
    fn ignores_prompt_example_marker_in_agent_end_and_prefers_real_verdict() {
        // Regression: an agent_end event embeds the full prompt (with its
        // example markers) plus the assistant's own message. The prompt
        // examples must not be mistaken for the verdict.
        let prompt_text = r#"instructions...\nHAMSTIK_WHEEL_RESULT={\"status\":\"ready_for_review\",\"summary\":\"brief summary\",\"findings\":[]}\nHAMSTIK_WHEEL_RESULT={\"status\":\"blocked\",\"summary\":\"why work cannot safely continue\",\"findings\":[\"blocking reason\"]}"#;
        let review_verdict = r#"HAMSTIK_WHEEL_RESULT={\"status\":\"blocked\",\"summary\":\"CLI-32 is not implemented at all\",\"findings\":[]}"#;
        let event = format!(
            r#"{{"type":"agent_end","messages":[{{"role":"system","content":""}},{{"role":"user","content":[{{"type":"text","text":"{prompt_example}"}}]}},{{"role":"assistant","content":[{{"type":"text","text":"{verdict}"}}]}}]}}"#,
            prompt_example = prompt_text,
            verdict = review_verdict,
        );
        let result = parse_agent_result(&event).unwrap();
        assert_eq!(result.status, "blocked");
        assert!(result.summary.contains("not implemented"));
    }

    #[test]
    fn rejects_transcript_with_only_prompt_example_markers() {
        let event = r#"{"type":"agent_end","messages":[{"role":"system","content":""},{"role":"user","content":[{"type":"text","text":"HAMSTIK_WHEEL_RESULT={\"status\":\"ready_for_review\",\"summary\":\"brief summary\",\"findings\":[]}"}]}]}"#;
        assert!(parse_agent_result(event).is_err());
    }

    #[test]
    fn ignores_example_markers_in_assistant_quoted_prompt() {
        // The assistant echoes the prompt examples inside its own thinking
        // but emits a real verdict in its text.
        let event = r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"HAMSTIK_WHEEL_RESULT={\"status\":\"ready_for_review\",\"summary\":\"brief summary\",\"findings\":[]}"},{"type":"text","text":"HAMSTIK_WHEEL_RESULT={\"status\":\"pass\",\"summary\":\"reviewed\",\"findings\":[]}"}]}}"#;
        let result = parse_agent_result(event).unwrap();
        assert_eq!(result.status, "pass");
        assert_eq!(result.summary, "reviewed");
    }

    #[test]
    fn parses_real_marker_after_placeholder_in_text() {
        // Assistant text contains a placeholder quote followed by the real
        // marker; the real (last) one wins.
        let event = r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"example: HAMSTIK_WHEEL_RESULT={\"status\":\"pass\",\"summary\":\"brief independent review summary\",\"findings\":[]}\nHAMSTIK_WHEEL_RESULT={\"status\":\"blocked\",\"summary\":\"real reason\",\"findings\":[\"x\"]}"}]}}"#;
        let result = parse_agent_result(event).unwrap();
        assert_eq!(result.status, "blocked");
        assert_eq!(result.summary, "real reason");
    }
}
