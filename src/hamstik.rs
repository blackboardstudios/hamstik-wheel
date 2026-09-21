// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{
    cmp::Ordering,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItemSummary {
    pub key: String,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSprint {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct HamstikCli {
    repo_root: std::path::PathBuf,
    cli_path: std::path::PathBuf,
}

impl HamstikCli {
    pub fn new(repo_root: &Path, cli_path: &str) -> Self {
        Self {
            repo_root: repo_root.to_path_buf(),
            cli_path: std::path::PathBuf::from(cli_path),
        }
    }

    fn run_json(&self, args: &[&str]) -> Result<Value> {
        let mut cmd = Command::new(&self.cli_path);
        cmd.current_dir(&self.repo_root)
            .args(["--no-input", "--json"])
            .args(args);
        let output = cmd.output().with_context(|| {
            format!(
                "failed to execute {} {}",
                self.cli_path.display(),
                args.join(" ")
            )
        })?;

        if !output.status.success() {
            bail!(
                "{} {} failed (exit {:?}): {}",
                self.cli_path.display(),
                args.join(" "),
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "{} {} did not return valid JSON",
                self.cli_path.display(),
                args.join(" ")
            )
        })
    }

    fn run_json_owned(&self, args: &[String]) -> Result<Value> {
        let mut cmd = Command::new(&self.cli_path);
        cmd.current_dir(&self.repo_root)
            .args(["--no-input", "--json"])
            .args(args);
        let output = cmd.output().with_context(|| {
            format!(
                "failed to execute {} {}",
                self.cli_path.display(),
                args.join(" ")
            )
        })?;

        if !output.status.success() {
            bail!(
                "{} {} failed (exit {:?}): {}",
                self.cli_path.display(),
                args.join(" "),
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "{} {} did not return valid JSON",
                self.cli_path.display(),
                args.join(" ")
            )
        })
    }

    pub fn doctor(&self) -> Result<Value> {
        self.run_json(&["doctor"])
    }

    pub fn command_manifest(&self) -> Result<Value> {
        self.run_json(&["commands"])
    }

    pub fn verify_required_commands(&self, require_assignment: bool) -> Result<()> {
        let mut required = vec![
            "hamstik doctor",
            "hamstik commands",
            "hamstik sprint list",
            "hamstik work list",
            "hamstik work context",
            "hamstik work start",
            "hamstik work close",
            "hamstik work transition",
            "hamstik work comment add",
        ];
        if require_assignment {
            required.push("hamstik work edit");
        }

        let manifest = self.command_manifest()?;
        let commands = manifest
            .get("commands")
            .and_then(Value::as_array)
            .context("hamstik command manifest is missing commands[]")?;

        for required in required {
            let entry = commands
                .iter()
                .find(|entry| entry.get("command").and_then(Value::as_str) == Some(required));
            let Some(entry) = entry else {
                bail!("installed hamstik CLI does not provide required command `{required}`");
            };
            let capabilities = entry.get("capabilities").and_then(Value::as_object);
            let json = capabilities
                .and_then(|c| c.get("json"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let no_input = capabilities
                .and_then(|c| c.get("noInput"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !json || !no_input {
                bail!("required command `{required}` does not advertise --json and --no-input capabilities");
            }
        }
        Ok(())
    }

    pub fn list_candidates(
        &self,
        statuses: &[String],
        item_types: &[String],
        label_names: &[String],
        sprint: Option<&str>,
    ) -> Result<Vec<WorkItemSummary>> {
        let mut args = vec!["work".to_string(), "list".to_string()];
        for status in statuses {
            args.push("--status".to_string());
            args.push(status.clone());
        }
        for item_type in item_types {
            args.push("--type".to_string());
            args.push(item_type.clone());
        }
        for label in label_names {
            args.push("--label-name".to_string());
            args.push(label.clone());
        }
        if let Some(id) = sprint {
            args.push("--sprint".to_string());
            args.push(id.to_string());
        }
        args.push("--all".to_string());
        let value = self.run_json_owned(&args)?;
        parse_candidates(&value)
    }

    /// The CLI resolves the same repository/project context as work list and
    /// follows all pages. Lifecycle state, not date arithmetic, is authoritative.
    pub fn active_sprints(&self) -> Result<Vec<ActiveSprint>> {
        let value = self.run_json(&["sprint", "list", "--all"])?;
        parse_active_sprints(&value)
    }

    /// Open Work Items assigned to the authenticated user in the given
    /// statuses. Used by doctor to detect items a crashed run stranded in
    /// `in_progress` — those are invisible to selection (which polls only the
    /// configured candidate statuses) until moved back manually.
    pub fn list_assigned_to_me(
        &self,
        statuses: &[&str],
        item_types: &[String],
    ) -> Result<Vec<WorkItemSummary>> {
        let mut args = vec![
            "work".to_string(),
            "list".to_string(),
            "--assignee".to_string(),
            "me".to_string(),
        ];
        for status in statuses {
            args.push("--status".to_string());
            args.push((*status).to_string());
        }
        for item_type in item_types {
            args.push("--type".to_string());
            args.push(item_type.clone());
        }
        args.push("--all".to_string());
        let value = self.run_json_owned(&args)?;
        parse_candidates(&value)
    }

    pub fn context(&self, key: &str) -> Result<Value> {
        self.run_json(&[
            "work",
            "context",
            key,
            "--comments",
            "100",
            "--activity",
            "25",
        ])
    }

    pub fn start(&self, key: &str) -> Result<Value> {
        self.run_json(&["work", "start", key])
    }

    pub fn assign_to_me(&self, key: &str) -> Result<Value> {
        self.run_json_owned(&assign_to_me_args(key))
    }

    pub fn close(&self, key: &str) -> Result<Value> {
        self.run_json(&["work", "close", key])
    }

    /// Current status name of a Work Item (`work view` returns the item with
    /// a plain `status` string). Used to guard the skip path's return-to-pool
    /// transition.
    pub fn item_status(&self, key: &str) -> Result<String> {
        let value = self.run_json(&["work", "view", key])?;
        Ok(named_value(value.get("status")).unwrap_or_default())
    }

    /// Transition an item to an arbitrary allowed target status (e.g. `todo`
    /// when a skipped item should return to the eligible pool).
    pub fn transition(&self, key: &str, target: &str) -> Result<Value> {
        self.run_json(&["work", "transition", key, target])
    }

    pub fn add_comment(
        &self,
        key: &str,
        body: &str,
        idempotency_key: Option<&str>,
    ) -> Result<Value> {
        let mut command = Command::new(&self.cli_path);
        command.current_dir(&self.repo_root).args([
            "--no-input",
            "--json",
            "work",
            "comment",
            "add",
            key,
            "--body-file",
            "-",
        ]);
        if let Some(value) = idempotency_key {
            command.args(["--idempotency-key", value]);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to execute hamstik work comment add")?;

        child
            .stdin
            .as_mut()
            .context("failed to open hamstik stdin")?
            .write_all(body.as_bytes())?;
        drop(child.stdin.take());
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!(
                "hamstik work comment add {} failed (exit {:?}): {}",
                key,
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        serde_json::from_slice(&output.stdout)
            .context("hamstik comment command did not return valid JSON")
    }
}

fn assign_to_me_args(key: &str) -> Vec<String> {
    vec![
        "work".to_string(),
        "edit".to_string(),
        key.to_string(),
        "--assignee".to_string(),
        "me".to_string(),
    ]
}

pub fn select_next(mut items: Vec<WorkItemSummary>) -> Option<WorkItemSummary> {
    items.sort_by(compare_candidates);
    items.into_iter().next()
}

fn compare_candidates(a: &WorkItemSummary, b: &WorkItemSummary) -> Ordering {
    status_rank(&a.status)
        .cmp(&status_rank(&b.status))
        .then_with(|| priority_rank(&a.priority).cmp(&priority_rank(&b.priority)))
        .then_with(|| compare_optional_created_at(&a.created_at, &b.created_at))
        .then_with(|| a.key.cmp(&b.key))
}

fn compare_optional_created_at(a: &Option<String>, b: &Option<String>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn normalize(s: &str) -> String {
    s.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

fn status_rank(status: &str) -> u8 {
    match normalize(status).as_str() {
        "in_progress" | "inprogress" => 0,
        _ => 1,
    }
}

fn priority_rank(priority: &str) -> u8 {
    match normalize(priority).as_str() {
        "urgent" | "critical" | "highest" => 0,
        "high" => 1,
        "medium" | "normal" => 2,
        "low" => 3,
        "none" | "no_priority" | "nopriority" => 4,
        _ => 5,
    }
}

fn parse_candidates(value: &Value) -> Result<Vec<WorkItemSummary>> {
    let array = locate_array(value)
        .context("could not locate Work Item array in hamstik work list JSON")?;
    let mut out = Vec::with_capacity(array.len());
    for item in array {
        if let Some(summary) = parse_summary(item) {
            out.push(summary);
        }
    }
    Ok(out)
}

fn parse_active_sprints(value: &Value) -> Result<Vec<ActiveSprint>> {
    let array =
        locate_array(value).context("could not locate Sprint array in hamstik sprint list JSON")?;
    let mut active = Vec::new();
    for sprint in array {
        let state = sprint
            .get("state")
            .and_then(Value::as_str)
            .filter(|state| !state.trim().is_empty())
            .context("hamstik sprint list returned a Sprint without a lifecycle state")?;
        let archived = ["archivedAt", "archived_at"]
            .iter()
            .any(|key| sprint.get(key).is_some_and(|value| !value.is_null()));
        if normalize(state) != "active" || archived {
            continue;
        }
        let id = sprint
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .context("hamstik sprint list returned an active Sprint without an id")?;
        active.push(ActiveSprint {
            id: id.to_string(),
            name: sprint
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .to_string(),
        });
    }
    active.sort_by(|a, b| a.id.cmp(&b.id));
    active.dedup_by(|a, b| a.id == b.id);
    Ok(active)
}

fn locate_array(value: &Value) -> Option<&Vec<Value>> {
    if let Value::Array(items) = value {
        return Some(items);
    }
    let obj = value.as_object()?;
    for key in ["items", "workItems", "work_items", "results"] {
        if let Some(Value::Array(items)) = obj.get(key) {
            return Some(items);
        }
    }
    if let Some(data) = obj.get("data") {
        if let Value::Array(items) = data {
            return Some(items);
        }
        if let Some(items) = locate_array(data) {
            return Some(items);
        }
    }
    None
}

fn parse_summary(value: &Value) -> Option<WorkItemSummary> {
    let obj = value.as_object()?;
    let key = scalar_string(obj.get("key"))?;
    let title = scalar_string(obj.get("title")).unwrap_or_else(|| key.clone());
    let status = named_value(obj.get("status")).unwrap_or_default();
    let priority = named_value(obj.get("priority")).unwrap_or_default();
    let created_at =
        scalar_string(obj.get("createdAt")).or_else(|| scalar_string(obj.get("created_at")));
    Some(WorkItemSummary {
        key,
        title,
        status,
        priority,
        created_at,
    })
}

fn scalar_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn named_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => ["key", "name", "slug", "value"]
            .iter()
            .find_map(|k| scalar_string(o.get(*k))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_unarchived_active_sprints_are_preferred() {
        let sprints = json!({"data":{"items":[
            {"id":"future","state":"future"},
            {"id":"done","state":"done"},
            {"id":"archived","state":"active","archivedAt":"2026-01-01"},
            {"id":"active","name":"Current sprint","state":"active","archivedAt":null},
            {"id":"active","name":"Current sprint","state":"active","archivedAt":null}
        ]}});
        assert_eq!(
            parse_active_sprints(&sprints).unwrap(),
            vec![ActiveSprint {
                id: "active".into(),
                name: "Current sprint".into()
            }]
        );
        assert!(parse_active_sprints(&json!([])).unwrap().is_empty());
    }

    #[test]
    fn malformed_sprint_discovery_is_not_treated_as_no_active_sprint() {
        for value in [
            json!({}),
            json!({"items":[{"id":"s1"}]}),
            json!({"items":[{"state":"active"}]}),
        ] {
            assert!(parse_active_sprints(&value).is_err());
        }
    }

    #[test]
    fn parses_common_envelope() {
        let value = json!({"data": {"items": [
            {"key":"HAM-2","title":"Two","status":"todo","priority":"high","createdAt":"2026-01-02"},
            {"key":"HAM-1","title":"One","status":{"name":"in_progress"},"priority":{"name":"low"},"createdAt":"2026-01-01"}
        ]}});
        let items = parse_candidates(&value).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(select_next(items).unwrap().key, "HAM-1");
    }

    #[test]
    fn priority_breaks_ties() {
        let items = vec![
            WorkItemSummary {
                key: "HAM-1".into(),
                title: "A".into(),
                status: "todo".into(),
                priority: "low".into(),
                created_at: None,
            },
            WorkItemSummary {
                key: "HAM-2".into(),
                title: "B".into(),
                status: "todo".into(),
                priority: "urgent".into(),
                created_at: None,
            },
        ];
        assert_eq!(select_next(items).unwrap().key, "HAM-2");
    }

    #[test]
    fn assignment_targets_authenticated_user() {
        assert_eq!(
            assign_to_me_args("HAM-42"),
            vec!["work", "edit", "HAM-42", "--assignee", "me"]
        );
    }
}
