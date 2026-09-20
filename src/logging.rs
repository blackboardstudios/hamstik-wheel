// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampMode {
    Local,
    Utc,
    None,
}

#[derive(Debug)]
pub struct Logger {
    timestamps: TimestampMode,
    log_file_path: Option<PathBuf>,
    verbose: bool,
    activity_enabled: bool,
    file: Mutex<Option<std::fs::File>>,
}

impl Logger {
    pub fn new(timestamps: TimestampMode, log_file: Option<&Path>, verbose: bool) -> Result<Self> {
        let file = match log_file {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(parent).with_context(|| {
                            format!("failed to create log directory {}", parent.display())
                        })?;
                    }
                }
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .with_context(|| format!("failed to open log file {}", path.display()))?;
                Some(file)
            }
            None => None,
        };
        let activity_enabled = log_file.is_none() && is_stdout_tty();
        Ok(Self {
            timestamps,
            log_file_path: log_file.map(Path::to_path_buf),
            verbose,
            activity_enabled,
            file: Mutex::new(file),
        })
    }

    pub fn timestamps(&self) -> TimestampMode {
        self.timestamps
    }

    pub fn log_file_path(&self) -> Option<PathBuf> {
        self.log_file_path.clone()
    }

    pub fn verbose(&self) -> bool {
        self.verbose
    }

    pub fn activity_enabled(&self) -> bool {
        self.activity_enabled
    }

    pub fn line(&self, stream: Stream, message: &str) {
        let stamp = self.render_stamp();
        let rendered = match stamp {
            Some(stamp) => format!("{stamp} {message}"),
            None => message.to_string(),
        };
        match stream {
            Stream::Stdout => println!("{rendered}"),
            Stream::Stderr => eprintln!("{rendered}"),
        }
        self.append_to_file(&rendered);
    }

    pub fn info(&self, message: &str) {
        self.line(Stream::Stdout, message);
    }

    pub fn warn(&self, message: &str) {
        self.line(Stream::Stderr, message);
    }

    pub fn error(&self, message: &str) {
        self.line(Stream::Stderr, message);
    }

    pub fn blank(&self) {
        println!();
        self.append_to_file("");
    }

    fn render_stamp(&self) -> Option<String> {
        match self.timestamps {
            TimestampMode::None => None,
            TimestampMode::Local => Some(format_local(Local::now())),
            TimestampMode::Utc => Some(format_utc(Utc::now())),
        }
    }

    fn append_to_file(&self, rendered: &str) {
        let mut guard = match self.file.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        let Some(file) = guard.as_mut() else { return };
        let _ = writeln!(file, "{rendered}");
        let _ = file.flush();
    }

    /// Mirror an already-printed multi-line blob (e.g. validation subprocess output)
    /// into the log file without re-printing or re-stamping it.
    pub fn append_raw(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut guard = match self.file.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        let Some(file) = guard.as_mut() else { return };
        let _ = file.write_all(text.as_bytes());
        if !text.ends_with('\n') {
            let _ = writeln!(file);
        }
        let _ = file.flush();
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Stream {
    Stdout,
    Stderr,
}

fn format_utc(now: DateTime<Utc>) -> String {
    format!("[{}]", now.format("%Y-%m-%dT%H:%M:%S%.3fZ"))
}

fn format_local(now: DateTime<Local>) -> String {
    format!("[{}]", now.format("%Y-%m-%dT%H:%M:%S%.3f%:z"))
}

/// Best-effort check whether stdout is an interactive terminal. In-place
/// activity rendering is disabled when it is not (piped output, CI, files).
#[cfg(target_os = "linux")]
fn is_stdout_tty() -> bool {
    // Reading the symlink /proc/self/fd/1 resolves to /dev/pts/N or /dev/tty
    // for terminals and to a path or "pipe:[...]" / "socket:[...]" otherwise.
    match std::fs::read_link("/proc/self/fd/1") {
        Ok(path) => {
            let text = path.to_string_lossy();
            text.starts_with("/dev/pts/")
                || text.starts_with("/dev/tty")
                || text.starts_with("/dev/console")
        }
        Err(_) => false,
    }
}

#[cfg(not(target_os = "linux"))]
fn is_stdout_tty() -> bool {
    // Non-Linux TTY detection is deferred; activity rendering stays off.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_formats_are_rfc3339_like() {
        let stamp = format_utc(Utc::now());
        assert!(stamp.starts_with('['));
        assert!(stamp.ends_with("Z]"));
        assert_eq!(stamp.len(), "[YYYY-MM-DDTHH:MM:SS.mmmZ]".len());

        let local = format_local(Local::now());
        assert!(local.starts_with('['));
        assert!(local.contains('T'));
    }
}
