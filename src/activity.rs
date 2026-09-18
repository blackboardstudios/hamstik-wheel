// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{sleep, JoinHandle},
    time::{Duration, Instant},
};

use serde_json::Value;

const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const TICK: Duration = Duration::from_millis(250);
const SUMMARY_LIMIT: usize = 48;

/// Shared state between the event reader and the redraw ticker.
struct Shared {
    description: Mutex<Option<String>>,
    done: AtomicBool,
}

/// Renders one in-place console line while an agent session runs, e.g.
/// `[pi] ⠸ bash: cargo test… · 0:03:12`. Disabled (never renders) when stdout
/// is not a terminal, so logs and CI captures stay clean.
pub struct ActivityTracker {
    shared: Arc<Shared>,
    start: Instant,
    worker: Option<JoinHandle<()>>,
    enabled: bool,
}

impl ActivityTracker {
    pub fn start(enabled: bool) -> Self {
        let shared = Arc::new(Shared {
            description: Mutex::new(None),
            done: AtomicBool::new(false),
        });
        let worker = enabled
            .then(|| {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name("pi-activity".to_string())
                    .spawn(move || render_loop(shared))
                    .ok()
            })
            .flatten();
        Self {
            shared,
            start: Instant::now(),
            worker,
            enabled,
        }
    }

    pub fn update(&self, description: Option<String>) {
        if let Ok(mut guard) = self.shared.description.lock() {
            *guard = description;
        }
    }

    pub fn elapsed(&self) -> std::time::Duration {
        self.start.elapsed()
    }

    /// Stop the ticker, erase the ephemeral line, and print one final
    /// terminal-activity line so the scrollback keeps a single record.
    pub fn finish(&mut self) {
        self.shared.done.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if !self.enabled {
            return;
        }
        let final_line = format!("[pi] ✓ finished · {}", format_elapsed(self.elapsed()));
        // Overwrite whatever partial frame is on screen, then keep the summary.
        print!("\r{final_line}\n");
        let _ = std::io::stdout().flush();
    }
}

impl Drop for ActivityTracker {
    fn drop(&mut self) {
        self.shared.done.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn render_loop(shared: Arc<Shared>) {
    let mut frame = 0usize;
    let start = Instant::now();
    while !shared.done.load(Ordering::Relaxed) {
        let description = shared
            .description
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        let text = match description {
            Some(description) => format!(
                "[pi] {} {} · {}",
                spinner(frame),
                description,
                format_elapsed(start.elapsed())
            ),
            None => format!(
                "[pi] {} working · {}",
                spinner(frame),
                format_elapsed(start.elapsed())
            ),
        };
        // Pad with spaces so a shorter description fully erases the previous frame.
        print!("\r{text:<80}");
        let _ = std::io::stdout().flush();
        frame = frame.wrapping_add(1);
        sleep(TICK);
    }
}

fn spinner(frame: usize) -> char {
    SPINNER_FRAMES[frame % SPINNER_FRAMES.len()]
}

fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    let (hours, minutes, seconds) = ((seconds / 3600), ((seconds % 3600) / 60), (seconds % 60));
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Map one NDJSON Pi event line to a compact activity description.
/// Returns `Some(Some(desc))` to change the line, `None` to leave the current
/// description untouched. Thinking arrives as a nested `assistantMessageEvent`
/// inside `message_update` events; tool execution as top-level events.
pub fn description_from_event(line: &str) -> Option<Option<String>> {
    let value: Value = serde_json::from_str(line).ok()?;
    match value.get("type").and_then(Value::as_str)? {
        "tool_execution_start" => {
            let tool = value
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let summary = value
                .get("args")
                .and_then(summarize_args)
                .map(|summary| format!(": {summary}"))
                .unwrap_or_default();
            Some(Some(format!("{tool}{summary}")))
        }
        "message_update" => {
            let event = value.get("assistantMessageEvent")?;
            match event.get("type").and_then(Value::as_str)? {
                "thinking_start" => Some(Some("thinking…".to_string())),
                "toolcall_start" => Some(Some(format!(
                    "calling {}…",
                    event
                        .get("toolName")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                ))),
                _ => None,
            }
        }
        _ => None,
    }
}

fn summarize_args(args: &Value) -> Option<String> {
    const KEYS: &[&str] = &[
        "command",
        "path",
        "file_path",
        "pattern",
        "query",
        "url",
        "description",
    ];
    let object = args.as_object()?;
    for key in KEYS {
        if let Some(Value::String(value)) = object.get(*key) {
            let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut summary: String = collapsed.chars().take(SUMMARY_LIMIT).collect();
            if collapsed.chars().count() > SUMMARY_LIMIT {
                summary.push('…');
            }
            return Some(summary);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_event_becomes_thinking_description() {
        let line = r#"{"type":"message_update","usage":{},"assistantMessageEvent":{"type":"thinking_start","contentIndex":0}}"#;
        assert_eq!(
            description_from_event(line),
            Some(Some("thinking…".to_string()))
        );
    }

    #[test]
    fn toolcall_start_becomes_calling_description() {
        let line = r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_start","toolName":"bash","contentIndex":1}}"#;
        assert_eq!(
            description_from_event(line),
            Some(Some("calling bash…".to_string()))
        );
    }

    #[test]
    fn tool_start_event_includes_command_summary() {
        let line = r#"{"type":"tool_execution_start","toolCallId":"x","toolName":"bash","args":{"command":"cargo test --workspace"}}"#;
        assert_eq!(
            description_from_event(line),
            Some(Some("bash: cargo test --workspace".to_string()))
        );
    }

    #[test]
    fn tool_start_event_includes_path_summary() {
        let line = r#"{"type":"tool_execution_start","toolCallId":"x","toolName":"write","args":{"path":"src/app.rs","content":"lots of text"}}"#;
        assert_eq!(
            description_from_event(line),
            Some(Some("write: src/app.rs".to_string()))
        );
    }

    #[test]
    fn other_events_do_not_change_description() {
        assert_eq!(
            description_from_event(r#"{"type":"tool_execution_end","toolName":"bash"}"#),
            None
        );
        assert_eq!(description_from_event(r#"{"type":"agent_start"}"#), None);
        assert_eq!(description_from_event("not json"), None);
    }

    #[test]
    fn long_commands_are_truncated() {
        let line = r#"{"type":"tool_execution_start","toolCallId":"x","toolName":"bash","args":{"command":"cargo build --release --features everything && cargo test --all-targets --all-features"}}"#;
        let Some(Some(description)) = description_from_event(line) else {
            panic!("expected description")
        };
        assert!(description.ends_with('…'));
        assert!(description.chars().count() <= "bash: ".len() + SUMMARY_LIMIT + 1);
    }

    #[test]
    fn elapsed_formats_minutes_and_hours() {
        assert_eq!(format_elapsed(Duration::from_secs(59)), "0:59");
        assert_eq!(format_elapsed(Duration::from_secs(61)), "1:01");
        assert_eq!(format_elapsed(Duration::from_secs(3700)), "1:01:40");
    }
}
