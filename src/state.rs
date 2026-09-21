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
    /// A skip was requested and tree/branch cleanup is in progress. Persisted
    /// before the cleanup starts so a crash mid-cleanup is recovered by the
    /// next run/resume instead of stranding the Work Item in `in_progress`.
    Skipping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveWorkItem {
    pub key: String,
    pub title: String,
    pub baseline_sha: String,
    #[serde(default)]
    pub commit_sha: Option<String>,
    #[serde(default)]
    pub validation_command_count: usize,
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
    /// Skip history per Work Item key, used to back off re-selection after
    /// repeated failures within one run and to warn about deterministic
    /// failures (same item, same model, same failure kind).
    #[serde(default)]
    pub skip_ledger: Vec<SkipRecord>,
    pub updated_at: DateTime<Utc>,
}

/// One recorded skip of a Work Item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkipRecord {
    pub key: String,
    pub title: String,
    /// Compact failure classification (e.g. `model-timeout`, `git-commit`).
    pub reason: String,
    /// Model that was active when the failure happened, when applicable.
    pub model: Option<String>,
    /// Immutable commit identity and branch of the most recent saved work.
    #[serde(default)]
    pub wip_branch: Option<String>,
    #[serde(default)]
    pub wip_sha: Option<String>,
    pub skipped_at: DateTime<Utc>,
}

impl WheelState {
    /// Most recent skip record for a key, if any.
    pub fn last_skip(&self, key: &str) -> Option<&SkipRecord> {
        self.skip_ledger
            .iter()
            .rev()
            .find(|record| record.key == key)
    }

    /// Append a skip record, keeping only the latest per key (bounded memory).
    pub fn record_skip(&mut self, record: SkipRecord) {
        self.skip_ledger
            .retain(|existing| existing.key != record.key);
        self.skip_ledger.push(record);
    }

    /// Remove the ledger entry for a key (e.g. after a successful completion).
    pub fn clear_skip(&mut self, key: &str) {
        self.skip_ledger.retain(|record| record.key != key);
    }
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
            skip_ledger: Vec::new(),
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

    /// Complete a skip: clear the active item and record the failure in the
    /// ledger. Only call after tree/branch cleanup has fully succeeded.
    pub fn finish_skip(&self, state: &mut WheelState, record: SkipRecord) -> Result<()> {
        state.phase = Phase::Idle;
        state.current = None;
        state.review_cycle = 0;
        state.last_error = Some(format!("{}: {}", record.key, record.reason));
        state.record_skip(record);
        self.save(state)
    }

    /// Reset the per-run completion counter at the start of a `run`
    /// invocation so `status` reflects the current run, not an accumulation
    /// across runs.
    pub fn begin_run(&self, state: &mut WheelState) -> Result<()> {
        state.completed_this_run = 0;
        self.save(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(key: &str) -> Option<ActiveWorkItem> {
        Some(ActiveWorkItem {
            key: key.to_string(),
            title: "Title".to_string(),
            baseline_sha: "abc".to_string(),
            commit_sha: None,
            validation_command_count: 0,
        })
    }

    #[test]
    fn state_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state.json"));
        let mut state = WheelState {
            phase: Phase::Selected,
            current: active("HAM-1"),
            ..WheelState::default()
        };
        store.save(&mut state).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.phase, Phase::Selected);
        assert_eq!(loaded.current.unwrap().key, "HAM-1");
    }

    #[test]
    fn skipping_phase_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state.json"));
        let mut state = WheelState {
            phase: Phase::Skipping,
            current: active("HAM-2"),
            ..WheelState::default()
        };
        store.save(&mut state).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.phase, Phase::Skipping);
        assert_eq!(loaded.current.unwrap().key, "HAM-2");
    }

    #[test]
    fn skip_ledger_keeps_latest_per_key() {
        let mut state = WheelState::default();
        state.record_skip(SkipRecord {
            key: "CLI-1".into(),
            title: "One".into(),
            reason: "model-timeout".into(),
            model: Some("m1".into()),
            wip_branch: None,
            wip_sha: None,
            skipped_at: Utc::now(),
        });
        state.record_skip(SkipRecord {
            key: "CLI-2".into(),
            title: "Two".into(),
            reason: "git-commit".into(),
            model: None,
            wip_branch: None,
            wip_sha: None,
            skipped_at: Utc::now(),
        });
        state.record_skip(SkipRecord {
            key: "CLI-1".into(),
            title: "One".into(),
            reason: "review-failed".into(),
            model: Some("m2".into()),
            wip_branch: None,
            wip_sha: None,
            skipped_at: Utc::now(),
        });
        assert_eq!(state.skip_ledger.len(), 2);
        assert_eq!(state.last_skip("CLI-1").unwrap().reason, "review-failed");
        state.clear_skip("CLI-1");
        assert!(state.last_skip("CLI-1").is_none());
        assert_eq!(state.skip_ledger.len(), 1);
    }

    #[test]
    fn skip_ledger_survives_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state.json"));
        let mut state = WheelState::default();
        state.record_skip(SkipRecord {
            key: "HAM-9".into(),
            title: "Nine".into(),
            reason: "model-timeout".into(),
            model: Some("slow-model".into()),
            wip_branch: None,
            wip_sha: None,
            skipped_at: Utc::now(),
        });
        store.save(&mut state).unwrap();
        let loaded = store.load().unwrap();
        let record = loaded.last_skip("HAM-9").unwrap();
        assert_eq!(record.reason, "model-timeout");
        assert_eq!(record.model.as_deref(), Some("slow-model"));
    }

    #[test]
    fn finish_skip_clears_active_and_records() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state.json"));
        let mut state = WheelState {
            phase: Phase::Skipping,
            current: active("HAM-3"),
            last_error: Some("boom".into()),
            ..WheelState::default()
        };
        store
            .finish_skip(
                &mut state,
                SkipRecord {
                    key: "HAM-3".into(),
                    title: "Title".into(),
                    reason: "implement-failed".into(),
                    model: Some("m".into()),
                    wip_branch: None,
                    wip_sha: None,
                    skipped_at: Utc::now(),
                },
            )
            .unwrap();
        assert_eq!(state.phase, Phase::Idle);
        assert!(state.current.is_none());
        assert_eq!(state.skip_ledger.len(), 1);
        let loaded = store.load().unwrap();
        assert_eq!(
            loaded.last_skip("HAM-3").unwrap().reason,
            "implement-failed"
        );
    }
}
