// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Selected,
    Claimed,
    Implementing,
    PreReviewValidation,
    Reviewing,
    FinalValidation,
    Committing,
    Closing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveWorkItem {
    pub key: String,
    pub title: String,
    pub baseline_sha: String,
    #[serde(default)]
    pub commit_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WheelState {
    pub schema_version: u32,
    pub phase: Phase,
    pub current: Option<ActiveWorkItem>,
    pub review_cycle: usize,
    pub completed_this_run: usize,
    pub last_error: Option<String>,
    pub updated_at: DateTime<Utc>,
}

impl Default for WheelState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            phase: Phase::Idle,
            current: None,
            review_cycle: 0,
            completed_this_run: 0,
            last_error: None,
            updated_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<WheelState> {
        if !self.path.exists() {
            return Ok(WheelState::default());
        }
        let raw = fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read state {}", self.path.display()))?;
        let state: WheelState = serde_json::from_str(&raw)
            .with_context(|| format!("state {} is invalid JSON", self.path.display()))?;
        if state.schema_version != 1 {
            bail!(
                "state {} uses unsupported schema version {} (expected 1)",
                self.path.display(),
                state.schema_version
            );
        }
        match (state.phase, state.current.is_some()) {
            (Phase::Idle, true) => bail!("idle state unexpectedly contains an active Work Item"),
            (phase, false) if phase != Phase::Idle => {
                bail!("state phase {phase:?} is missing its active Work Item")
            }
            _ => {}
        }
        Ok(state)
    }

    pub fn save(&self, state: &mut WheelState) -> Result<()> {
        state.updated_at = Utc::now();
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let raw = serde_json::to_vec_pretty(state)?;
        fs::write(&tmp, raw)
            .with_context(|| format!("failed to write temporary state {}", tmp.display()))?;
        #[cfg(windows)]
        if self.path.exists() {
            fs::remove_file(&self.path).with_context(|| {
                format!("failed to replace existing state {}", self.path.display())
            })?;
        }
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to replace state {}", self.path.display()))?;
        Ok(())
    }

    pub fn clear_active(&self, state: &mut WheelState) -> Result<()> {
        state.phase = Phase::Idle;
        state.current = None;
        state.review_cycle = 0;
        state.last_error = None;
        self.save(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state.json"));
        let mut state = WheelState {
            phase: Phase::Selected,
            current: Some(ActiveWorkItem {
                key: "HAM-1".into(),
                title: "One".into(),
                baseline_sha: "abc".into(),
                commit_sha: None,
            }),
            ..WheelState::default()
        };
        store.save(&mut state).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.phase, Phase::Selected);
        assert_eq!(loaded.current.unwrap().key, "HAM-1");
    }
}
