// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};

use crate::{
    config::{self, Config},
    git::GitRepo,
    hamstik::{select_next, HamstikCli},
    logging::Logger,
    pi::{implementation_prompt, review_prompt, PiRunner},
    state::{ActiveWorkItem, Phase, StateStore, WheelState},
    validation::run_all,
};

pub struct LoopEngine {
    repo: GitRepo,
    config: Config,
    hamstik: HamstikCli,
    pi: PiRunner,
    store: StateStore,
    logs_root: PathBuf,
    logger: Logger,
}

impl LoopEngine {
    pub fn load_with_logger(logger: &Logger) -> Result<Self> {
        let repo = GitRepo::discover()?;
        let config = Config::load(repo.root())?;
        let hamstik = HamstikCli::new(repo.root(), &config.hamstik.cli_path);
        let pi = PiRunner::new(repo.root());
        let state_path = repo.metadata_path("hamstik-wheel/state.json")?;
        let logs_root = repo.metadata_path("hamstik-wheel/logs")?;
        Ok(Self {
            repo,
            config,
            hamstik,
            pi,
            store: StateStore::new(state_path),
            logs_root,
            logger: Logger::new(logger.timestamps(), logger.log_file_path().as_deref())?,
        })
    }

    pub fn doctor(&self) -> Result<()> {
        let mut failed = false;
        self.logger.info("Hamstik Wheel Doctor");
        self.logger.blank();

        failed |= !check_process(&self.logger, "git", &["--version"]);
        self.logger
            .info(&format!("✓ Git repository: {}", self.repo.root().display()));
        self.logger.info(&format!(
            "✓ Configuration: {}",
            Config::path(self.repo.root()).display()
        ));
        failed |= !check_process(&self.logger, "pi", &["--version"]);
        failed |= !check_process(&self.logger, &self.config.hamstik.cli_path, &["--version"]);

        match self.hamstik.verify_required_commands() {
            Ok(()) => self
                .logger
                .info("✓ Hamstik CLI command manifest supports Wheel requirements"),
            Err(e) => {
                self.logger
                    .error(&format!("✗ Hamstik CLI command surface: {e:#}"));
                failed = true;
            }
        }
        match self.hamstik.doctor() {
            Ok(_) => self.logger.info("✓ Hamstik CLI doctor"),
            Err(e) => {
                self.logger.error(&format!("✗ Hamstik CLI doctor: {e:#}"));
                failed = true;
            }
        }

        let state = self.store.load()?;
        if state.current.is_none() && self.config.git.require_clean_start {
            match self.repo.is_clean() {
                Ok(true) => self.logger.info("✓ Working tree clean"),
                Ok(false) => {
                    self.logger
                        .error("✗ Working tree is dirty and require_clean_start=true");
                    failed = true;
                }
                Err(e) => {
                    self.logger
                        .error(&format!("✗ Could not inspect working tree: {e:#}"));
                    failed = true;
                }
            }
        } else if state.current.is_some() {
            self.logger
                .info("✓ Active Wheel state present; dirty tree allowed for resume");
        }

        if self.config.validation.commands.is_empty() {
            self.logger
                .warn("! No validation commands configured; final gate will rely on review only");
        } else {
            self.logger.info(&format!(
                "✓ {} validation command(s) configured",
                self.config.validation.commands.len()
            ));
        }

        if check_pi_model(&self.config.models.implement) {
            self.logger.info(&format!(
                "✓ Implementation model available: {}",
                self.config.models.implement
            ));
        } else {
            self.logger.error(&format!(
                "✗ Pi could not find implementation model: {}",
                self.config.models.implement
            ));
            failed = true;
        }
        if check_pi_model(&self.config.models.review) {
            self.logger.info(&format!(
                "✓ Review model available: {}",
                self.config.models.review
            ));
        } else {
            self.logger.error(&format!(
                "✗ Pi could not find review model: {}",
                self.config.models.review
            ));
            failed = true;
        }

        if failed {
            bail!("doctor found one or more blocking problems");
        }
        self.logger.blank();
        self.logger.info("Ready.");
        Ok(())
    }

    pub fn status(&self) -> Result<()> {
        let state = self.store.load()?;
        self.logger
            .info(&format!("State file: {}", self.store.path().display()));
        self.logger
            .info(&format!("Logs: {}", self.logs_root.display()));
        self.logger.info(&format!("Phase: {:?}", state.phase));
        if let Some(item) = state.current.as_ref() {
            self.logger
                .info(&format!("Work Item: {} — {}", item.key, item.title));
            self.logger
                .info(&format!("Baseline: {}", item.baseline_sha));
            self.logger
                .info(&format!("Review cycle: {}", state.review_cycle));
        } else {
            self.logger.info("Work Item: none");
        }
        if let Some(error) = state.last_error.as_ref() {
            self.logger.info(&format!("Last error: {error}"));
        }
        Ok(())
    }

    pub fn run(&self, max_items: Option<usize>) -> Result<()> {
        self.preflight()?;
        let limit = max_items.unwrap_or(self.config.r#loop.max_items);
        if limit == 0 {
            bail!("max-items must be greater than zero");
        }

        let mut completed = 0usize;
        let mut skipped = 0usize;
        while completed + skipped < limit {
            match self.process_one()? {
                ProcessOutcome::Completed => {
                    completed += 1;
                    self.logger.blank();
                    self.logger
                        .info(&format!("Completed {completed}/{limit} Work Item(s)."));
                    self.logger.blank();
                }
                ProcessOutcome::Skipped => {
                    skipped += 1;
                    self.logger.blank();
                    self.logger.info(&format!(
                        "Skipped {skipped}/{limit} Work Item(s) after failure; continuing (on_failure = skip)."
                    ));
                    self.logger.blank();
                }
                ProcessOutcome::NoWork => {
                    self.logger.info("No eligible Hamstik Work Items found.");
                    break;
                }
            }
        }
        if skipped > 0 {
            self.logger
                .warn(&format!("Run finished with {skipped} skipped Work Item(s); see the per-item comments and state for details."));
        }
        Ok(())
    }

    pub fn once(&self) -> Result<()> {
        self.preflight()?;
        match self.process_one()? {
            ProcessOutcome::Completed | ProcessOutcome::Skipped => Ok(()),
            ProcessOutcome::NoWork => {
                self.logger.info("No eligible Hamstik Work Items found.");
                Ok(())
            }
        }
    }

    pub fn resume(&self) -> Result<()> {
        let state = self.store.load()?;
        if state.current.is_none() {
            bail!("there is no active Hamstik Wheel state to resume");
        }
        self.once()
    }

    fn preflight(&self) -> Result<()> {
        require_process("pi", &["--version"])?;
        require_process(&self.config.hamstik.cli_path, &["--version"])?;
        self.hamstik.verify_required_commands()?;
        self.hamstik.doctor()?;
        if !check_pi_model(&self.config.models.implement) {
            bail!(
                "Pi could not resolve implementation model `{}`. If it is ambiguous across providers, set the config value to a provider-qualified id like `provider/model` (run `pi --list-models {}` to see matches).",
                self.config.models.implement,
                self.config.models.implement
            );
        }
        if !check_pi_model(&self.config.models.review) {
            bail!(
                "Pi could not resolve review model `{}`. If it is ambiguous across providers, set the config value to a provider-qualified id like `provider/model` (run `pi --list-models {}` to see matches).",
                self.config.models.review,
                self.config.models.review
            );
        }
        if !pi_supports_json_mode() {
            bail!(
                "Pi does not support `--mode json`; activity streaming requires it. \
                 Update Pi to a version with JSON output mode."
            );
        }
        Ok(())
    }

    fn process_one(&self) -> Result<ProcessOutcome> {
        let mut state = self.store.load()?;

        if state.current.is_none() {
            if self.config.git.require_clean_start && !self.repo.is_clean()? {
                bail!("working tree is dirty; finish/stash existing work before Hamstik Wheel selects a new Work Item");
            }
            let candidates = self.hamstik.list_candidates(
                &self.config.hamstik.statuses,
                &self.config.hamstik.item_types,
                &self.config.hamstik.label_names,
            )?;
            let Some(item) = select_next(candidates) else {
                return Ok(ProcessOutcome::NoWork);
            };
            let baseline = self.repo.head()?;
            self.logger
                .info(&format!("Selected {} — {}", item.key, item.title));
            state.current = Some(ActiveWorkItem {
                key: item.key,
                title: item.title,
                baseline_sha: baseline,
                commit_sha: None,
            });
            state.phase = Phase::Selected;
            state.review_cycle = 0;
            state.last_error = None;
            self.store.save(&mut state)?;
        }

        if let Err(error) = self.process_active(&mut state) {
            let error_text = format!("{error:#}");
            state.last_error = Some(error_text.clone());
            let _ = self.store.save(&mut state);

            if self.config.r#loop.on_failure == config::OnFailure::Skip {
                let (key, title, baseline) = match state.current.as_ref() {
                    Some(active) => (
                        active.key.clone(),
                        active.title.clone(),
                        active.baseline_sha.clone(),
                    ),
                    None => (String::new(), String::new(), String::new()),
                };
                if !key.is_empty() {
                    // Preserve the session's in-progress changes on a WIP
                    // branch, then restore the baseline so the next
                    // selection's clean-tree check passes.
                    let wip_branch = match self.repo.preserve_wip(&key, &baseline) {
                        Ok(branch) => branch,
                        Err(preserve_error) => {
                            state.last_error = Some(format!(
                                "{error_text}; WIP preservation failed: {preserve_error:#}"
                            ));
                            let _ = self.store.save(&mut state);
                            self.logger.error(&format!(
                                "[skip] {} could not preserve in-progress changes: {preserve_error:#}; halting the run",
                                key
                            ));
                            return Err(anyhow::anyhow!(error_text)
                                .context("skipped-item WIP preservation failed"));
                        }
                    };
                    if let Some(branch) = wip_branch.as_deref() {
                        self.logger.info(&format!(
                            "[cleanup] {} in-progress changes preserved on branch `{branch}`",
                            key
                        ));
                    }
                    self.record_skip(&key, &title, &baseline, &error_text, wip_branch.as_deref())?;
                    self.reopen_item(&key, &title);
                    self.store.clear_active(&mut state)?;
                    self.logger.info(&format!(
                        "[skip] {} recorded; item returned to the eligible pool; continuing",
                        key
                    ));
                    return Ok(ProcessOutcome::Skipped);
                }
            }
            return Err(error);
        }

        Ok(ProcessOutcome::Completed)
    }

    fn process_active(&self, state: &mut WheelState) -> Result<()> {
        let current = state
            .current
            .clone()
            .context("active state is missing current Work Item")?;
        if state.phase == Phase::Idle {
            bail!("active Work Item cannot have idle phase");
        }

        // Read the authoritative bundle on every resumed invocation. This keeps agent input
        // current while Hamstik CLI remains the sole owner of Hamstik API semantics.
        let context = self.hamstik.context(&current.key)?;

        if state.phase == Phase::Selected {
            self.logger.info(&format!("[claim] {}", current.key));
            self.hamstik.start(&current.key)?;
            if self.config.comments.post_started {
                let body = format!(
                    "Hamstik Wheel started automated implementation.\n\n- Baseline: `{}`\n- Implementation model: `{}`\n- Review model: `{}`",
                    current.baseline_sha, self.config.models.implement, self.config.models.review
                );
                let idem = comment_idempotency_key("start", &current.key, &current.baseline_sha);
                self.hamstik.add_comment(&current.key, &body, Some(&idem))?;
            }
            state.phase = Phase::Claimed;
            self.store.save(state)?;
        }

        if matches!(state.phase, Phase::Claimed | Phase::Implementing) {
            state.phase = Phase::Implementing;
            self.store.save(state)?;
            self.logger.blank();
            self.logger.info(&format!(
                "[implement] {} with {}",
                current.key, self.config.models.implement
            ));
            let prompt = implementation_prompt(
                &current.key,
                &current.title,
                &current.baseline_sha,
                &context,
            );
            let run = self.run_implement_session(&current.key, &prompt)?;
            self.write_log(&current.key, "implement.log", &run.transcript)?;
            let result = run.result.with_context(|| {
                format!(
                    "implementation agent failed; transcript: '{}'; state preserved, run `hamstik-wheel resume` to continue {}",
                    self.log_path(&current.key, "implement.log").display(),
                    current.key
                )
            })?;
            match result.status.as_str() {
                "ready_for_review" => {}
                "blocked" => bail!("implementation blocked: {}", result.summary),
                other => bail!("unexpected implementation result status: {other}"),
            }
            state.phase = Phase::PreReviewValidation;
            self.store.save(state)?;
        }

        if matches!(
            state.phase,
            Phase::PreReviewValidation | Phase::Reviewing | Phase::FinalValidation
        ) {
            let mut evidence = if state.phase == Phase::PreReviewValidation {
                self.logger.blank();
                self.logger.info("[validate] pre-review");
                let report = run_all(
                    self.repo.root(),
                    &self.config.validation.commands,
                    &self.logger,
                )?;
                let full_evidence = report.evidence();
                self.write_log(&current.key, "pre-review-validation.log", &full_evidence)?;
                // Bounded digest for the prompt: full output can exceed the
                // review model's context window (observed: 49K-token overflow).
                report.evidence_digest(2000)
            } else {
                "This is a resumed review phase. Independently inspect the current working tree and rerun relevant checks; do not assume an earlier review result still applies.".to_string()
            };

            let starting_cycle = state.review_cycle.max(1);
            let mut passed = false;
            for cycle in starting_cycle..=self.config.r#loop.max_review_cycles {
                state.review_cycle = cycle;
                state.phase = Phase::Reviewing;
                self.store.save(state)?;
                self.logger.blank();
                self.logger.info(&format!(
                    "[review] cycle {cycle}/{} with {}",
                    self.config.r#loop.max_review_cycles, self.config.models.review
                ));
                let prompt = review_prompt(
                    &current.key,
                    &current.title,
                    &current.baseline_sha,
                    &context,
                    &evidence,
                    cycle,
                );
                let run = self.run_agent(&self.config.models.review, &prompt)?;
                let review_log = format!("review-{cycle:02}.log");
                self.write_log(&current.key, &review_log, &run.transcript)?;
                let review = run.result.with_context(|| {
                    format!(
                        "review agent failed (cycle {cycle}); transcript: '{}'; state preserved, run `hamstik-wheel resume` to continue {}",
                        self.log_path(&current.key, &review_log).display(),
                        current.key
                    )
                })?;
                if review.status == "blocked" {
                    bail!("review blocked: {}", review.summary);
                }
                if review.status != "pass" {
                    evidence = format!(
                        "Reviewer returned status {}. Findings:\n{}",
                        review.status,
                        review.findings.join("\n")
                    );
                    continue;
                }
                if !review.findings.is_empty() {
                    evidence = format!(
                        "Reviewer claimed PASS but still reported findings:\n{}",
                        review.findings.join("\n")
                    );
                    continue;
                }

                state.phase = Phase::FinalValidation;
                self.store.save(state)?;
                self.logger.blank();
                self.logger.info(&format!("[validate] final cycle {cycle}"));
                let final_report = run_all(
                    self.repo.root(),
                    &self.config.validation.commands,
                    &self.logger,
                )?;
                let final_evidence = final_report.evidence();
                self.write_log(
                    &current.key,
                    &format!("final-validation-{cycle:02}.log"),
                    &final_evidence,
                )?;
                if final_report.passed {
                    passed = true;
                    break;
                }
                evidence = final_report.evidence_digest(2000);
            }

            if !passed {
                bail!(
                    "review/validation did not reach PASS within {} cycle(s)",
                    self.config.r#loop.max_review_cycles
                );
            }

            if self.config.git.commit {
                state.phase = Phase::Committing;
            } else {
                // An intentionally uncommitted run remains represented by a null commit SHA.
                // This avoids implying that the baseline HEAD contains the implementation.
                if let Some(active) = state.current.as_mut() {
                    active.commit_sha = None;
                }
                state.phase = Phase::Closing;
            }
            self.store.save(state)?;
        }

        if state.phase == Phase::Committing {
            let commit_sha = if let Some(existing) = state
                .current
                .as_ref()
                .and_then(|item| item.commit_sha.clone())
            {
                existing
            } else if self.repo.is_clean()? && self.repo.head()? != current.baseline_sha {
                // A crash may have occurred immediately after a successful commit and before
                // state persistence. Since agents are explicitly forbidden from committing,
                // a clean tree with HEAD advanced from the baseline is the conservative
                // recovery signal for the Wheel-created commit.
                self.repo.head()?
            } else {
                let message = GitRepo::expand_commit_message(
                    &self.config.git.commit_message,
                    &current.key,
                    &current.title,
                );
                self.logger.blank();
                self.logger.info(&format!("[commit] {message}"));
                self.repo.commit_all(&message)?
            };

            if let Some(active) = state.current.as_mut() {
                active.commit_sha = Some(commit_sha);
            }
            state.phase = Phase::Closing;
            self.store.save(state)?;
        }

        if state.phase == Phase::Closing {
            let commit_sha = state
                .current
                .as_ref()
                .and_then(|item| item.commit_sha.clone());
            if self.config.git.commit && commit_sha.is_none() {
                bail!("commit-enabled run reached closing phase without a recorded commit SHA");
            }

            if self.config.comments.post_completed {
                let body = completion_comment(
                    commit_sha.as_deref(),
                    &self.config.models.implement,
                    &self.config.models.review,
                    state.review_cycle,
                    self.config.validation.commands.len(),
                );
                let idem = comment_idempotency_key("complete", &current.key, &current.baseline_sha);
                self.hamstik.add_comment(&current.key, &body, Some(&idem))?;
            }
            self.logger.blank();
            self.logger
                .info(&format!("[complete] closing {}", current.key));
            self.hamstik.close(&current.key)?;

            state.completed_this_run += 1;
            self.store.clear_active(state)?;
            match commit_sha.as_deref() {
                Some(sha) => self
                    .logger
                    .info(&format!("{} complete at {}", current.key, sha)),
                None => self.logger.info(&format!(
                    "{} complete (changes left uncommitted by configuration)",
                    current.key
                )),
            }
            return Ok(());
        }

        bail!(
            "unexpected terminal phase {:?} for {}",
            state.phase,
            current.key
        )
    }

    fn log_path(&self, key: &str, name: &str) -> PathBuf {
        self.logs_root.join(sanitize_component(key)).join(name)
    }

    fn write_log(&self, key: &str, name: &str, content: &str) -> Result<()> {
        let path = self.log_path(key, name);
        fs::create_dir_all(path.parent().context("log path has no parent directory")?)?;
        fs::write(path, content)?;
        Ok(())
    }

    fn agent_timeout(&self) -> Option<std::time::Duration> {
        let minutes = self.config.r#loop.agent_timeout_minutes;
        (minutes > 0).then(|| std::time::Duration::from_secs(minutes * 60))
    }

    fn run_agent(&self, model: &str, prompt: &str) -> Result<crate::pi::PiRun> {
        self.pi
            .run(model, prompt, self.agent_timeout(), &self.logger)
    }

    /// Run one implementation session, with an optional single automatic
    /// retry when the first session fails to produce a parsable result
    /// marker (process errors and timeouts are also retried once; explicit
    /// `blocked` results never are).
    fn run_implement_session(&self, key: &str, prompt: &str) -> Result<crate::pi::PiRun> {
        let mut run = self.run_agent(&self.config.models.implement, prompt)?;
        if run.result.is_err() && self.config.r#loop.implement_retry {
            let retry_log = "implement-retry.log";
            self.logger.warn(&format!(
                "[implement] {key}: first session did not produce a parsable result; retrying once"
            ));
            let retry = self.run_agent(&self.config.models.implement, prompt)?;
            self.write_log(key, retry_log, &retry.transcript)?;
            if retry.result.is_ok() {
                run = retry;
            } else {
                return Err(run
                    .result
                    .err()
                    .map(|error| {
                        anyhow::anyhow!(error.to_string())
                            .context("implementation agent failed (after one retry)")
                    })
                    .unwrap_or_else(|| {
                        anyhow::anyhow!("implementation agent failed (after one retry)")
                    }));
            }
        }
        Ok(run)
    }

    /// Record a failure comment on the Work Item so the run's history shows
    /// what happened overnight, then clear active state (skip path).
    fn record_skip(
        &self,
        key: &str,
        title: &str,
        baseline: &str,
        error: &str,
        wip_branch: Option<&str>,
    ) -> Result<()> {
        self.logger.blank();
        self.logger.warn(&format!("[skip] {key}: {error}"));
        let wip_note = match wip_branch {
            Some(branch) => format!("\n- Partial in-progress changes preserved on branch `{branch}` (inspect with `git diff {baseline} {branch}`)"),
            None => String::new(),
        };
        let body = format!(
            "Hamstik Wheel could not complete automated work for this Work Item and moved on (on_failure = skip).\n\n- Failure: {error}\n- Baseline: `{baseline}`{wip_note}\n- The item was returned to `todo` and stays eligible for later runs"
        );
        let idem = comment_idempotency_key("skip", key, baseline);
        if let Err(comment_error) = self.hamstik.add_comment(key, &body, Some(&idem)) {
            self.logger.warn(&format!(
                "[skip] could not post failure comment on {title}: {comment_error:#}"
            ));
        }
        Ok(())
    }

    /// Best-effort transition of a skipped item back to `todo` so it remains
    /// in the eligible pool for later runs. Failures are non-fatal: the skip
    /// already recorded the outcome, and a manual transition is possible.
    fn reopen_item(&self, key: &str, title: &str) {
        if let Err(error) = self.hamstik.transition(key, "todo") {
            self.logger.warn(&format!(
                "[skip] could not return {title} to todo ({error:#}); move it back manually"
            ));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessOutcome {
    Completed,
    Skipped,
    NoWork,
}

fn require_process(name: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(name)
        .args(args)
        .output()
        .with_context(|| format!("failed to execute required dependency `{name}`"))?;
    if !output.status.success() {
        bail!(
            "required dependency `{name}` exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn check_process(logger: &Logger, name: &str, args: &[&str]) -> bool {
    match Command::new(name).args(args).output() {
        Ok(output) if output.status.success() => {
            let first = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .to_string();
            logger.info(&format!(
                "✓ {name}: {}",
                if first.is_empty() {
                    "available"
                } else {
                    first.as_str()
                }
            ));
            true
        }
        Ok(output) => {
            logger.error(&format!("✗ {name}: exited {:?}", output.status.code()));
            false
        }
        Err(error) => {
            logger.error(&format!("✗ {name}: {error}"));
            false
        }
    }
}

/// Verify Pi can resolve a model id to exactly one provider/model pair, the
/// way Wheel's agent invocations will. Uses a short real invocation (the
/// cheapest reliable signal: bare patterns ambiguous across authenticated
/// providers and unresolvable ids both fail at startup, before any request).
fn check_pi_model(model: &str) -> bool {
    use std::io::Write;
    let Ok(mut child) = Command::new("pi")
        .args(["--model", model, "--no-session", "--mode", "json", "-p"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    // An empty prompt resolves instantly at startup; any response is harmless.
    let _ = child.stdin.as_mut().map(|stdin| stdin.write_all(b""));
    drop(child.stdin.take());
    match child.wait() {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

/// Check that the installed Pi supports the JSON output mode Wheel uses for
/// activity streaming and structured marker extraction.
fn pi_supports_json_mode() -> bool {
    Command::new("pi")
        .args(["--mode", "json", "--help"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn completion_comment(
    commit_sha: Option<&str>,
    implement_model: &str,
    review_model: &str,
    cycles: usize,
    validation_commands: usize,
) -> String {
    let commit = commit_sha
        .map(|sha| format!("`{sha}`"))
        .unwrap_or_else(|| "not created (commit disabled)".to_string());
    format!(
        "Hamstik Wheel completed automated implementation.\n\n- Commit: {commit}\n- Implementation model: `{implement_model}`\n- Review model: `{review_model}`\n- Review cycles: {cycles}\n- Independent review: **PASS**\n- Final repository validation: **PASS** ({validation_commands} configured command(s))\n- Outstanding actionable findings: 0"
    )
}

fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn comment_idempotency_key(kind: &str, key: &str, baseline: &str) -> String {
    let short = baseline.chars().take(12).collect::<String>();
    format!("hamstik-wheel-{kind}-{}-{short}", sanitize_component(key))
}
