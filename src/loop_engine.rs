// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{fs, path::PathBuf, process::Command};

use anyhow::{bail, Context, Result};

use crate::{
    config::Config,
    git::GitRepo,
    hamstik::{select_next, HamstikCli},
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
}

impl LoopEngine {
    pub fn load() -> Result<Self> {
        let repo = GitRepo::discover()?;
        let config = Config::load(repo.root())?;
        let hamstik = HamstikCli::new(repo.root(), &config.hamstik.cli_path);
        let pi = PiRunner::new(repo.root());
        let state_path = repo.metadata_path("hamstik-wheel/state.json")?;
        let logs_root = repo.metadata_path("hamstik-wheel/logs")?;
        Ok(Self { repo, config, hamstik, pi, store: StateStore::new(state_path), logs_root })
    }

    pub fn doctor(&self) -> Result<()> {
        let mut failed = false;
        println!("Hamstik Wheel Doctor\n");

        failed |= !check_process("git", &["--version"]);
        println!("✓ Git repository: {}", self.repo.root().display());
        println!("✓ Configuration: {}", Config::path(self.repo.root()).display());
        failed |= !check_process("pi", &["--version"]);
        failed |= !check_process(&self.config.hamstik.cli_path, &["--version"]);

        match self.hamstik.verify_required_commands() {
            Ok(()) => println!("✓ Hamstik CLI command manifest supports Wheel requirements"),
            Err(e) => { eprintln!("✗ Hamstik CLI command surface: {e:#}"); failed = true; }
        }
        match self.hamstik.doctor() {
            Ok(_) => println!("✓ Hamstik CLI doctor"),
            Err(e) => { eprintln!("✗ Hamstik CLI doctor: {e:#}"); failed = true; }
        }

        let state = self.store.load()?;
        if state.current.is_none() && self.config.git.require_clean_start {
            match self.repo.is_clean() {
                Ok(true) => println!("✓ Working tree clean"),
                Ok(false) => { eprintln!("✗ Working tree is dirty and require_clean_start=true"); failed = true; }
                Err(e) => { eprintln!("✗ Could not inspect working tree: {e:#}"); failed = true; }
            }
        } else if state.current.is_some() {
            println!("✓ Active Wheel state present; dirty tree allowed for resume");
        }

        if self.config.validation.commands.is_empty() {
            println!("! No validation commands configured; final gate will rely on review only");
        } else {
            println!("✓ {} validation command(s) configured", self.config.validation.commands.len());
        }

        if check_pi_model(&self.config.models.implement) {
            println!("✓ Implementation model available: {}", self.config.models.implement);
        } else {
            eprintln!("✗ Pi could not find implementation model: {}", self.config.models.implement);
            failed = true;
        }
        if check_pi_model(&self.config.models.review) {
            println!("✓ Review model available: {}", self.config.models.review);
        } else {
            eprintln!("✗ Pi could not find review model: {}", self.config.models.review);
            failed = true;
        }

        if failed { bail!("doctor found one or more blocking problems"); }
        println!("\nReady.");
        Ok(())
    }

    pub fn status(&self) -> Result<()> {
        let state = self.store.load()?;
        println!("State file: {}", self.store.path().display());
        println!("Logs: {}", self.logs_root.display());
        println!("Phase: {:?}", state.phase);
        if let Some(item) = state.current.as_ref() {
            println!("Work Item: {} — {}", item.key, item.title);
            println!("Baseline: {}", item.baseline_sha);
            println!("Review cycle: {}", state.review_cycle);
        } else {
            println!("Work Item: none");
        }
        if let Some(error) = state.last_error.as_ref() {
            println!("Last error: {error}");
        }
        Ok(())
    }

    pub fn run(&self, max_items: Option<usize>) -> Result<()> {
        self.preflight()?;
        let limit = max_items.unwrap_or(self.config.r#loop.max_items);
        if limit == 0 { bail!("max-items must be greater than zero"); }

        let mut completed = 0usize;
        while completed < limit {
            match self.process_one()? {
                ProcessOutcome::Completed => {
                    completed += 1;
                    println!("\nCompleted {completed}/{limit} Work Item(s).\n");
                }
                ProcessOutcome::NoWork => {
                    println!("No eligible Hamstik Work Items found.");
                    break;
                }
            }
        }
        Ok(())
    }

    pub fn once(&self) -> Result<()> {
        self.preflight()?;
        match self.process_one()? {
            ProcessOutcome::Completed => Ok(()),
            ProcessOutcome::NoWork => { println!("No eligible Hamstik Work Items found."); Ok(()) }
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
            bail!("Pi could not resolve implementation model `{}`", self.config.models.implement);
        }
        if !check_pi_model(&self.config.models.review) {
            bail!("Pi could not resolve review model `{}`", self.config.models.review);
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
            let Some(item) = select_next(candidates) else { return Ok(ProcessOutcome::NoWork); };
            let baseline = self.repo.head()?;
            println!("Selected {} — {}", item.key, item.title);
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
            state.last_error = Some(format!("{error:#}"));
            let _ = self.store.save(&mut state);
            return Err(error);
        }

        Ok(ProcessOutcome::Completed)
    }

    fn process_active(&self, state: &mut WheelState) -> Result<()> {
        let current = state.current.clone().context("active state is missing current Work Item")?;
        if state.phase == Phase::Idle {
            bail!("active Work Item cannot have idle phase");
        }

        // Read the authoritative bundle on every resumed invocation. This keeps agent input
        // current while Hamstik CLI remains the sole owner of Hamstik API semantics.
        let context = self.hamstik.context(&current.key)?;

        if state.phase == Phase::Selected {
            println!("[claim] {}", current.key);
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
            println!("\n[implement] {} with {}", current.key, self.config.models.implement);
            let prompt = implementation_prompt(&current.key, &current.title, &current.baseline_sha, &context);
            let run = self.pi.run(&self.config.models.implement, &prompt)?;
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

        if matches!(state.phase, Phase::PreReviewValidation | Phase::Reviewing | Phase::FinalValidation) {
            let mut evidence = if state.phase == Phase::PreReviewValidation {
                println!("\n[validate] pre-review");
                let report = run_all(self.repo.root(), &self.config.validation.commands)?;
                let evidence = report.evidence();
                self.write_log(&current.key, "pre-review-validation.log", &evidence)?;
                evidence
            } else {
                "This is a resumed review phase. Independently inspect the current working tree and rerun relevant checks; do not assume an earlier review result still applies.".to_string()
            };

            let starting_cycle = state.review_cycle.max(1);
            let mut passed = false;
            for cycle in starting_cycle..=self.config.r#loop.max_review_cycles {
                state.review_cycle = cycle;
                state.phase = Phase::Reviewing;
                self.store.save(state)?;
                println!("\n[review] cycle {cycle}/{} with {}", self.config.r#loop.max_review_cycles, self.config.models.review);
                let prompt = review_prompt(
                    &current.key,
                    &current.title,
                    &current.baseline_sha,
                    &context,
                    &evidence,
                    cycle,
                );
                let run = self.pi.run(&self.config.models.review, &prompt)?;
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
                    evidence = format!("Reviewer returned status {}. Findings:\n{}", review.status, review.findings.join("\n"));
                    continue;
                }
                if !review.findings.is_empty() {
                    evidence = format!("Reviewer claimed PASS but still reported findings:\n{}", review.findings.join("\n"));
                    continue;
                }

                state.phase = Phase::FinalValidation;
                self.store.save(state)?;
                println!("\n[validate] final cycle {cycle}");
                let final_report = run_all(self.repo.root(), &self.config.validation.commands)?;
                let final_evidence = final_report.evidence();
                self.write_log(&current.key, &format!("final-validation-{cycle:02}.log"), &final_evidence)?;
                if final_report.passed {
                    passed = true;
                    break;
                }
                evidence = final_evidence;
            }

            if !passed {
                bail!("review/validation did not reach PASS within {} cycle(s)", self.config.r#loop.max_review_cycles);
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
            let commit_sha = if let Some(existing) = state.current.as_ref().and_then(|item| item.commit_sha.clone()) {
                existing
            } else if self.repo.is_clean()? && self.repo.head()? != current.baseline_sha {
                // A crash may have occurred immediately after a successful commit and before
                // state persistence. Since agents are explicitly forbidden from committing,
                // a clean tree with HEAD advanced from the baseline is the conservative
                // recovery signal for the Wheel-created commit.
                self.repo.head()?
            } else {
                let message = GitRepo::expand_commit_message(&self.config.git.commit_message, &current.key, &current.title);
                println!("\n[commit] {message}");
                self.repo.commit_all(&message)?
            };

            if let Some(active) = state.current.as_mut() {
                active.commit_sha = Some(commit_sha);
            }
            state.phase = Phase::Closing;
            self.store.save(state)?;
        }

        if state.phase == Phase::Closing {
            let commit_sha = state.current.as_ref().and_then(|item| item.commit_sha.clone());
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
            println!("\n[complete] closing {}", current.key);
            self.hamstik.close(&current.key)?;

            state.completed_this_run += 1;
            self.store.clear_active(state)?;
            match commit_sha.as_deref() {
                Some(sha) => println!("{} complete at {}", current.key, sha),
                None => println!("{} complete (changes left uncommitted by configuration)", current.key),
            }
            return Ok(());
        }

        bail!("unexpected terminal phase {:?} for {}", state.phase, current.key)
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessOutcome { Completed, NoWork }

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

fn check_process(name: &str, args: &[&str]) -> bool {
    match Command::new(name).args(args).output() {
        Ok(output) if output.status.success() => {
            let first = String::from_utf8_lossy(&output.stdout).lines().next().unwrap_or_default().to_string();
            println!("✓ {name}: {}", if first.is_empty() { "available" } else { first.as_str() });
            true
        }
        Ok(output) => {
            eprintln!("✗ {name}: exited {:?}", output.status.code());
            false
        }
        Err(error) => {
            eprintln!("✗ {name}: {error}");
            false
        }
    }
}

fn check_pi_model(model: &str) -> bool {
    match Command::new("pi").args(["--list-models", model]).output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            !stdout.trim().is_empty() && stdout.to_ascii_lowercase().contains(&model.to_ascii_lowercase())
        }
        _ => false,
    }
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
    value.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect()
}

fn comment_idempotency_key(kind: &str, key: &str, baseline: &str) -> String {
    let short = baseline.chars().take(12).collect::<String>();
    format!("hamstik-wheel-{kind}-{}-{short}", sanitize_component(key))
}
