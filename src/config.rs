// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const CONFIG_FILE: &str = ".hamstik-wheel.toml";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub hamstik: HamstikConfig,
    pub models: ModelsConfig,
    pub validation: ValidationConfig,
    pub r#loop: LoopConfig,
    pub git: GitConfig,
    pub comments: CommentsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HamstikConfig {
    pub cli_path: String,
    pub statuses: Vec<String>,
    pub item_types: Vec<String>,
    pub label_names: Vec<String>,
    /// Assign each claimed Work Item to the authenticated Hamstik user.
    pub assign_to_me: bool,
}

impl Default for HamstikConfig {
    fn default() -> Self {
        Self {
            cli_path: "hamstik".to_string(),
            statuses: vec!["todo".to_string()],
            item_types: vec![
                "task".to_string(),
                "bug".to_string(),
                "story".to_string(),
                "feature".to_string(),
            ],
            label_names: Vec::new(),
            assign_to_me: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelsConfig {
    pub implement: String,
    pub review: String,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            implement: "step-3.7-flash".to_string(),
            review: "glm-5.3-flash".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ValidationConfig {
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    /// Stop the run when a Work Item fails (current default).
    #[default]
    Halt,
    /// Record the failure, restore the baseline working tree, and continue
    /// with the next Work Item.
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoopConfig {
    pub max_items: usize,
    pub max_review_cycles: usize,
    pub on_failure: OnFailure,
    /// Wall-clock cap for each agent (implement/review) session, in minutes.
    /// Zero disables the cap.
    pub agent_timeout_minutes: u64,
    /// One extra implementation attempt when the first session fails to
    /// produce a parsable result marker.
    pub implement_retry: bool,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_items: 10,
            max_review_cycles: 3,
            on_failure: OnFailure::Halt,
            agent_timeout_minutes: 45,
            implement_retry: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GitConfig {
    pub require_clean_start: bool,
    pub commit: bool,
    pub commit_message: String,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            require_clean_start: true,
            commit: true,
            commit_message: "{key}: {title}".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CommentsConfig {
    pub post_started: bool,
    pub post_completed: bool,
}

impl Default for CommentsConfig {
    fn default() -> Self {
        Self {
            post_started: true,
            post_completed: true,
        }
    }
}

impl Config {
    pub fn path(repo_root: &Path) -> PathBuf {
        repo_root.join(CONFIG_FILE)
    }

    pub fn load(repo_root: &Path) -> Result<Self> {
        let path = Self::path(repo_root);
        let raw = fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to read {} (run `hamstik-wheel init` first)",
                path.display()
            )
        })?;
        let config: Config =
            toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        const STATUSES: &[&str] = &["backlog", "todo", "in_progress", "in_review", "done"];
        const TYPES: &[&str] = &["task", "bug", "story", "feature", "epic"];
        if self.hamstik.statuses.is_empty() {
            bail!("[hamstik].statuses must contain at least one status");
        }
        for status in &self.hamstik.statuses {
            if !STATUSES.contains(&status.as_str()) {
                bail!("unsupported [hamstik].statuses value `{status}`");
            }
        }
        for item_type in &self.hamstik.item_types {
            if !TYPES.contains(&item_type.as_str()) {
                bail!("unsupported [hamstik].item_types value `{item_type}`");
            }
        }
        if self.hamstik.cli_path.trim().is_empty() {
            bail!("[hamstik].cli_path must not be empty");
        }
        if self.models.implement.trim().is_empty() {
            bail!("[models].implement must not be empty");
        }
        if self.models.review.trim().is_empty() {
            bail!("[models].review must not be empty");
        }
        if self.r#loop.max_items == 0 {
            bail!("[loop].max_items must be greater than zero");
        }
        if self.r#loop.max_review_cycles == 0 {
            bail!("[loop].max_review_cycles must be greater than zero");
        }
        Ok(())
    }

    pub fn init(repo_root: &Path) -> Result<PathBuf> {
        let path = Self::path(repo_root);
        if path.exists() {
            bail!("{} already exists", path.display());
        }
        let config = Config::default();
        let raw =
            toml::to_string_pretty(&config).context("failed to render default configuration")?;
        fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_initial_model_pair() {
        let c = Config::default();
        assert_eq!(c.models.implement, "step-3.7-flash");
        assert_eq!(c.models.review, "glm-5.3-flash");
        assert_eq!(c.hamstik.statuses, vec!["todo"]);
        assert!(!c.hamstik.item_types.contains(&"epic".to_string()));
        assert!(c.hamstik.assign_to_me);
    }

    #[test]
    fn default_config_round_trips() {
        let raw = toml::to_string(&Config::default()).unwrap();
        let parsed: Config = toml::from_str(&raw).unwrap();
        parsed.validate().unwrap();
    }

    #[test]
    fn assignment_can_be_disabled() {
        let parsed: Config = toml::from_str("[hamstik]\nassign_to_me = false\n").unwrap();
        parsed.validate().unwrap();
        assert!(!parsed.hamstik.assign_to_me);
    }

    #[test]
    fn on_failure_accepts_skip_and_halt() {
        for value in ["halt", "skip"] {
            let parsed: Config =
                toml::from_str(&format!("[loop]\non_failure = \"{value}\"\n")).unwrap();
            parsed.validate().unwrap();
        }
        assert!(toml::from_str::<Config>("[loop]\non_failure = \"restart\"\n").is_err());
    }

    #[test]
    fn overnight_settings_round_trip() {
        let raw = r#"
[loop]
max_items = 5
max_review_cycles = 3
on_failure = "skip"
agent_timeout_minutes = 60
implement_retry = true
"#;
        let parsed: Config = toml::from_str(raw).unwrap();
        parsed.validate().unwrap();
        assert_eq!(parsed.r#loop.on_failure, OnFailure::Skip);
        assert_eq!(parsed.r#loop.agent_timeout_minutes, 60);
        assert!(parsed.r#loop.implement_retry);
    }

    #[test]
    fn zero_agent_timeout_disables_cap() {
        let parsed: Config = toml::from_str("[loop]\nagent_timeout_minutes = 0\n").unwrap();
        parsed.validate().unwrap();
        assert_eq!(parsed.r#loop.agent_timeout_minutes, 0);
    }
}
