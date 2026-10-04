// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    git::GitRepo,
    logging::Logger,
    security::Redactor,
    state::{Phase, StateStore},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Selected,
    Started,
    Reviewing,
    Validating,
    Validated,
    Committing,
    Closing,
    Completed,
    Failed,
    Skipping,
    Skipped,
    Deferred,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Selected => "Selected",
            Self::Started => "Started",
            Self::Reviewing => "Reviewing",
            Self::Validating => "Validating",
            Self::Validated => "Validated",
            Self::Committing => "Committing",
            Self::Closing => "Closing",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Skipping => "Skipping",
            Self::Skipped => "Skipped",
            Self::Deferred => "Deferred",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Event {
    pub timestamp: DateTime<Utc>,
    pub kind: Kind,
    pub key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub detail: String,
}

/// Lifecycle facts belong to Wheel, not model-generated transcripts. In
/// particular, Completed is recorded only after the close command succeeds.
pub fn append(logs: &Path, event: &Event, redactor: &Redactor) -> Result<()> {
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(logs)?;
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(logs.join("progress.jsonl"))?;
    let safe = Event {
        timestamp: event.timestamp,
        kind: event.kind,
        key: redactor.text(&event.key),
        title: redactor.text(&event.title),
        detail: redactor.text(&event.detail),
    };
    // A leading separator also allows recovery from a torn previous append.
    let record = format!("\n{}\n", serde_json::to_string(&safe)?);
    file.write_all(record.as_bytes())?;
    file.sync_data()?;
    Ok(())
}

#[derive(Default)]
struct Report {
    items: BTreeMap<String, Event>,
    damaged_records: usize,
    legacy: bool,
}

impl Report {
    fn observe(&mut self, event: Event) {
        if self
            .items
            .get(&event.key)
            .is_none_or(|previous| event.timestamp >= previous.timestamp)
        {
            self.items.insert(event.key.clone(), event);
        }
    }

    fn read(logs: &Path, state_path: &Path) -> Result<Self> {
        let mut report = Self::default();
        // Old transcripts establish activity, never authoritative completion.
        for entry in entries(logs)? {
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let key = entry.file_name().to_string_lossy().into_owned();
            let mut files = entries(&entry.path())?;
            files.extend(entries(&entry.path().join("history"))?);
            for file in files {
                if !file.file_type()?.is_file() {
                    continue;
                }
                let name = file.file_name().to_string_lossy().into_owned();
                // Archived legacy aliases have copy times, not activity times.
                if name.starts_with("legacy-") || !name.ends_with(".log") {
                    continue;
                }
                let (kind, detail) = if name == "implement.log" || name.ends_with("-implement.log")
                {
                    (Kind::Started, "implementation log recorded".to_string())
                } else if name.starts_with("review-")
                    || name.contains("-review-") && !name.contains("pre-review")
                {
                    (Kind::Reviewing, "review log recorded".to_string())
                } else if name.starts_with("final-validation-")
                    || name.contains("-final-validation-")
                {
                    let mut first_line = String::new();
                    BufReader::new(File::open(file.path())?).read_line(&mut first_line)?;
                    match first_line.trim() {
                        "overall: PASS" => (
                            Kind::Validated,
                            "final validation passed; completion not recorded".to_string(),
                        ),
                        "overall: FAIL" => (Kind::Failed, "final validation failed".to_string()),
                        _ => (
                            Kind::Validating,
                            "validation log recorded; outcome unknown".to_string(),
                        ),
                    }
                } else if name.ends_with("pre-review-validation.log") {
                    (
                        Kind::Validating,
                        "pre-review validation log recorded".to_string(),
                    )
                } else {
                    continue;
                };
                report.observe(Event {
                    timestamp: file.metadata()?.modified()?.into(),
                    kind,
                    key: key.clone(),
                    title: String::new(),
                    detail,
                });
            }
        }
        // Journal records are authoritative for items written by new Wheel
        // versions. Alias copies must not replace them based on filesystem time.
        let journal = logs.join("progress.jsonl");
        let mut recorded = Report::default();
        match File::open(&journal) {
            Ok(file) => {
                for line in BufReader::new(file).lines() {
                    let line = line?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Event>(&line) {
                        // Append order is authoritative even if the system
                        // clock moves backwards between attempts.
                        Ok(event) => {
                            recorded.items.insert(event.key.clone(), event);
                        }
                        Err(_) => report.damaged_records += 1,
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("failed to read progress journal"),
        }
        report.legacy = report
            .items
            .keys()
            .any(|key| !recorded.items.contains_key(key));
        report.items.extend(recorded.items);

        let state = StateStore::new(state_path.to_path_buf()).load()?;
        for skip in state.skip_ledger {
            // An active checkpoint is a subsequent attempt. Its old skip
            // ledger entry may remain until success, even across clock changes.
            if state
                .current
                .as_ref()
                .is_some_and(|item| item.key == skip.key)
            {
                continue;
            }
            report.observe(Event {
                timestamp: skip.skipped_at,
                kind: Kind::Skipped,
                key: skip.key,
                title: skip.title,
                detail: skip.reason,
            });
        }
        if let Some(current) = state.current {
            let kind = match state.phase {
                Phase::Idle => unreachable!("StateStore rejects idle states with an active item"),
                Phase::Selected => Kind::Selected,
                Phase::Claimed | Phase::Implementing => Kind::Started,
                Phase::PreReviewValidation | Phase::FinalValidation => Kind::Validating,
                Phase::Reviewing => Kind::Reviewing,
                Phase::Committing => Kind::Committing,
                Phase::Closing => Kind::Closing,
                Phase::Skipping => Kind::Skipping,
            };
            report.observe(Event {
                timestamp: state.updated_at,
                kind: if state.last_error.is_some() {
                    Kind::Failed
                } else {
                    kind
                },
                key: current.key,
                title: current.title,
                detail: match state.last_error {
                    Some(error) => format!("checkpoint {:?}: {error}", state.phase),
                    None => format!(
                        "checkpoint {:?}; process may be running or interrupted",
                        state.phase
                    ),
                },
            });
        }
        Ok(report)
    }
}

fn entries(path: &Path) -> Result<Vec<fs::DirEntry>> {
    match fs::read_dir(path) {
        Ok(entries) => entries
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(Into::into),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn single_line(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect()
}

pub fn report(repo: &GitRepo, logger: &Logger) -> Result<()> {
    let logs = repo.metadata_path("hamstik-wheel/logs")?;
    let report = Report::read(&logs, &repo.metadata_path("hamstik-wheel/state.json")?)?;
    let redactor = Redactor::new(repo.root());
    if report.damaged_records > 0 {
        logger.warn(&format!(
            "Ignored {} incomplete or invalid progress record(s).",
            report.damaged_records
        ));
    }
    if report.items.is_empty() {
        logger.info("No recorded Hamstik Wheel progress in this repository.");
        return Ok(());
    }
    logger.info("Latest recorded progress (oldest activity first):");
    let mut events: Vec<_> = report.items.into_values().collect();
    events.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.key.cmp(&b.key)));
    for event in events {
        let mut line = format!("{} {}", event.kind.label(), single_line(&event.key));
        if !event.title.is_empty() {
            line.push_str(&format!(" — {}", single_line(&redactor.text(&event.title))));
        }
        if !event.detail.is_empty() {
            line.push_str(&format!(
                " ({})",
                single_line(&redactor.text(&event.detail))
            ));
        }
        logger.info(&line);
    }
    if report.legacy {
        logger.info("Older logs show recorded stages; validation or review success alone does not confirm completion.");
    }
    Ok(())
}
