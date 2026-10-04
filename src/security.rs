// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{fs, path::Path, process::Command};

pub const TOOLS: &str = "wheel_read,wheel_write,wheel_edit,wheel_bash,wheel_result";

pub fn prepare_runtime(
    root: &Path,
    directory: &Path,
    read_only_paths: &[String],
) -> Result<String> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)?;
    fs::write(
        directory.join("sandbox.mjs"),
        include_str!("../runtime/sandbox.mjs"),
    )?;
    fs::write(
        directory.join("guard.mjs"),
        include_str!("../runtime/guard.mjs"),
    )?;
    fs::write(
        directory.join("protocol.mjs"),
        include_str!("../runtime/protocol.mjs"),
    )?;
    let output = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()?;
    if !output.status.success() {
        bail!("cannot locate Git metadata for sandbox");
    }
    let git_path = String::from_utf8(output.stdout)?.trim().to_string();
    Ok(json!({"root": root.canonicalize()?, "gitPaths": [git_path], "readOnlyPaths": read_only_paths}).to_string())
}

pub fn check_runtime(directory: &Path, config: &str) -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("Wheel requires Linux and bubblewrap for agent tool isolation");
    }
    let output = Command::new("node")
        .arg(directory.join("sandbox.mjs"))
        .arg("--check")
        .env("HAMSTIK_WHEEL_SANDBOX", config)
        .output()
        .context("sandbox requires Node.js and /usr/bin/bwrap")?;
    if !output.status.success() {
        bail!(
            "sandbox preflight failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct Redactor {
    secrets: Vec<String>,
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    [
        "PASSWORD",
        "SECRET",
        "TOKEN",
        "API_KEY",
        "APIKEY",
        "PRIVATEKEY",
        "AUTHORIZATION",
        "DATABASE_URL",
    ]
    .iter()
    .any(|p| key.contains(p))
}

impl Redactor {
    pub fn new(root: &Path) -> Self {
        let mut secrets: Vec<_> = std::env::vars()
            .filter(|(key, value)| sensitive_key(key) && value.len() >= 6)
            .map(|(_, value)| value)
            .collect();
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(".env") {
                    if let Ok(contents) = fs::read_to_string(entry.path()) {
                        for line in contents.lines() {
                            if let Some((key, value)) = line.split_once('=') {
                                let value = value.trim().trim_matches(['\'', '"']);
                                if sensitive_key(key) && value.len() >= 6 {
                                    secrets.push(value.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Self { secrets }
    }

    pub fn text(&self, text: &str) -> String {
        let mut clean = text.to_string();
        for secret in &self.secrets {
            clean = clean.replace(secret, "[REDACTED]");
        }
        // Remove URI userinfo, including credentials not present in the environment.
        let mut cursor = 0;
        while let Some(relative) = clean[cursor..].find("://") {
            let start = cursor + relative + 3;
            let end = clean[start..]
                .find(['/', ' ', '\n', '\r', '\t', '"', '\''])
                .map(|n| start + n)
                .unwrap_or(clean.len());
            if let Some(at) = clean[start..end].rfind('@') {
                clean.replace_range(start..start + at, "[REDACTED]");
                cursor = start + "[REDACTED]".len() + 1;
            } else {
                cursor = start;
            }
        }
        clean
    }

    pub fn event(&self, line: &str) -> Option<String> {
        if let Ok(mut value) = serde_json::from_str::<Value>(line) {
            // Partial deltas can split a secret across lines and are redundant
            // with complete message/tool events. Do not persist them.
            if matches!(
                value.get("type").and_then(Value::as_str),
                Some("message_update" | "tool_execution_update")
            ) {
                return None;
            }
            self.value(&mut value);
            Some(value.to_string())
        } else {
            Some(self.text(line))
        }
    }

    fn value(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.text(text),
            Value::Array(items) => items.iter_mut().for_each(|v| self.value(v)),
            Value::Object(map) => {
                for (key, value) in map {
                    if sensitive_key(key) && value.is_string() {
                        *value = Value::String("[REDACTED]".into());
                    } else {
                        self.value(value);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redacts_credentials_without_breaking_json_and_omits_deltas() {
        let r = Redactor {
            secrets: vec!["private-value".into()],
        };
        let line = json!({"type":"message_end","message":{"content":[{"text":"postgresql://alice:p%40ss@localhost/db private-value"}]}}).to_string();
        let clean = r.event(&line).unwrap();
        assert!(!clean.contains("p%40ss") && !clean.contains("private-value"));
        assert!(serde_json::from_str::<Value>(&clean).is_ok());
        assert!(r
            .event(r#"{"type":"message_update","delta":"priv"}"#)
            .is_none());
        assert_eq!(
            r.text("https://example.test/path"),
            "https://example.test/path"
        );
    }
}
