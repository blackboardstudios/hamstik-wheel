// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{fs, path::{Path, PathBuf}};

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
    pub statuses: Vec<String>,
    pub item_types: Vec<String>,
    pub label_names: Vec<String>,
}

impl Default for HamstikConfig {
    fn default() -> Self {
        Self {
            statuses: vec!["todo".to_string()],
            item_types: vec![
                "task".to_string(),
                "bug".to_string(),
                "story".to_string(),
                "feature".to_string(),
            ],
            label_names: Vec::new(),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoopConfig {
    pub max_items: usize,
    pub max_review_cycles: usize,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self { max_items: 10, max_review_cycles: 3 }
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
        Self { post_started: true, post_completed: true }
    }
}

impl Config {
    pub fn path(repo_root: &Path) -> PathBuf {
        repo_root.join(CONFIG_FILE)
    }

    pub fn load(repo_root: &Path) -> Result<Self> {
        let path = Self::path(repo_root);
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {} (run `hamstik-wheel init` first)", path.display()))?;
        let config: Config = toml::from_str(&raw)
            .with_context(|| format!("failed to parse {}", path.display()))?;
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
        let raw = toml::to_string_pretty(&config).context("failed to render default configuration")?;
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
    }

    #[test]
    fn default_config_round_trips() {
        let raw = toml::to_string(&Config::default()).unwrap();
        let parsed: Config = toml::from_str(&raw).unwrap();
        parsed.validate().unwrap();
    }
}
