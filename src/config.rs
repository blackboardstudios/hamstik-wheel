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
    pub sandbox: SandboxConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SandboxConfig {
    /// Additional toolchain directories mounted read-only in tool sandboxes.
    pub read_only_paths: Vec<String>,
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
    /// Additional CLI attempts after a confirmed Hamstik API rate limit.
    pub rate_limit_retries: usize,
    /// Initial delay; doubles per retry, capped at five minutes.
    pub rate_limit_retry_delay_seconds: u64,
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
            rate_limit_retries: 3,
            rate_limit_retry_delay_seconds: 30,
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
    pub execution: ValidationExecution,
    /// Lightweight commands that prove the configured validation sandbox can
    /// start before Wheel claims a Work Item. These are not test substitutes.
    pub preflight_commands: Vec<String>,
    pub commands: Vec<String>,
    pub rules: Vec<ValidationRule>,
    /// Paths that must be covered by a specialized validation rule.
    pub require_rules_for: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ValidationExecution {
    #[default]
    Sandbox,
    /// Explicit operator trust: commands execute agent-modified code on the host.
    TrustedHost,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationRule {
    /// Repository-relative literal prefixes (no glob syntax).
    pub path_prefixes: Vec<String>,
    pub commands: Vec<String>,
}

impl ValidationConfig {
    pub fn require_coverage(&self, paths: &[String]) -> Result<()> {
        for path in paths {
            if self
                .require_rules_for
                .iter()
                .any(|prefix| path.starts_with(prefix))
                && !self.rules.iter().any(|rule| {
                    rule.path_prefixes
                        .iter()
                        .any(|prefix| path.starts_with(prefix))
                })
            {
                bail!("required validation rule missing for changed path {path}");
            }
        }
        Ok(())
    }

    pub fn commands_for_paths(&self, paths: &[String]) -> Vec<String> {
        let mut commands = self.commands.clone();
        for rule in &self.rules {
            if paths.iter().any(|path| {
                rule.path_prefixes
                    .iter()
                    .any(|prefix| path.starts_with(prefix))
            }) {
                for command in &rule.commands {
                    if !commands.contains(command) {
                        commands.push(command.clone());
                    }
                }
            }
        }
        commands
    }
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
    /// Stop an unattended run after this many consecutive item skips.
    pub max_consecutive_skips: usize,
    /// Wall-clock cap for each agent (implement/review) session, in minutes.
    /// Zero disables the cap.
    pub agent_timeout_minutes: u64,
    /// One extra implementation attempt when the first session fails to
    /// produce a parsable result marker.
    pub implement_retry: bool,
    /// Additional sessions for transient provider errors; independent of the
    /// one retry for process/protocol failures.
    pub provider_retries: usize,
    /// Initial delay; doubles per provider retry, capped at five minutes.
    pub provider_retry_delay_seconds: u64,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_items: 10,
            max_review_cycles: 3,
            on_failure: OnFailure::Halt,
            max_consecutive_skips: 3,
            agent_timeout_minutes: 45,
            implement_retry: true,
            provider_retries: 3,
            provider_retry_delay_seconds: 30,
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
        if self.r#loop.max_consecutive_skips == 0 {
            bail!("[loop].max_consecutive_skips must be greater than zero");
        }
        if self.r#loop.provider_retries > 10 {
            bail!("[loop].provider_retries must be at most 10");
        }
        if self.hamstik.rate_limit_retries > 10 {
            bail!("[hamstik].rate_limit_retries must be at most 10");
        }
        for rule in &self.validation.rules {
            if rule.path_prefixes.is_empty() || rule.commands.is_empty() {
                bail!("validation rules require path_prefixes and commands");
            }
            for prefix in &rule.path_prefixes {
                if prefix.trim().is_empty()
                    || prefix.starts_with('/')
                    || prefix.contains("..")
                    || prefix.starts_with("./")
                    || prefix.contains(['*', '?', '\\', ':'])
                {
                    bail!("validation path prefix `{prefix}` must be a repository-relative literal prefix");
                }
            }
        }
        for prefix in &self.validation.require_rules_for {
            if prefix.trim().is_empty()
                || prefix.starts_with('/')
                || prefix.contains("..")
                || prefix.starts_with("./")
                || prefix.contains(['*', '?', '\\', ':'])
            {
                bail!("validation require_rules_for entries must be repository-relative literal prefixes");
            }
        }
        for path in &self.sandbox.read_only_paths {
            if !Path::new(path).is_absolute() {
                bail!("sandbox read_only_paths must be absolute directories");
            }
        }
        if self
            .validation
            .preflight_commands
            .iter()
            .chain(self.validation.commands.iter())
            .chain(self.validation.rules.iter().flat_map(|r| &r.commands))
            .any(|command| command.trim().is_empty())
        {
            bail!("validation commands must not be empty");
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
    fn validation_rules_add_required_commands_only_for_matching_paths() {
        let parsed: Config = toml::from_str(
            r#"
[validation]
commands = ["precheck"]
[[validation.rules]]
path_prefixes = ["drizzle/", "src/db/"]
commands = ["precheck", "migration-test", "postgres-test"]
"#,
        )
        .unwrap();
        parsed.validate().unwrap();
        assert_eq!(
            parsed
                .validation
                .commands_for_paths(&["docs/index.md".into()]),
            vec!["precheck"]
        );
        assert_eq!(
            parsed
                .validation
                .commands_for_paths(&["drizzle/0064.sql".into(), "src/db/schema.ts".into()]),
            vec!["precheck", "migration-test", "postgres-test"]
        );
    }

    #[test]
    fn rejects_invalid_rules_and_unbounded_retry_count() {
        for raw in [
            "[[validation.rules]]\npath_prefixes = []\ncommands = ['test']",
            "[[validation.rules]]\npath_prefixes = ['../drizzle']\ncommands = ['test']",
            "[[validation.rules]]\npath_prefixes = ['drizzle/']\ncommands = ['']",
            "[loop]\nprovider_retries = 11",
            "[hamstik]\nrate_limit_retries = 11",
        ] {
            assert!(toml::from_str::<Config>(raw).unwrap().validate().is_err());
        }
    }

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

    #[test]
    fn rejects_zero_consecutive_skip_limit() {
        let parsed: Config = toml::from_str("[loop]\nmax_consecutive_skips = 0\n").unwrap();
        assert!(parsed.validate().is_err());
    }
}
