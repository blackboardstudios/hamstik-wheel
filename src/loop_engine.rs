// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};

use crate::{
    config::{self, Config},
    git::GitRepo,
    hamstik::{select_next, HamstikCli},
    logging::Logger,
    pi::{implementation_prompt, review_prompt, PiRunner, ProviderFailure},
    state::{ActiveWorkItem, Phase, SkipRecord, StateStore, WheelState},
    validation::{run_all, ValidationKind},
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
            logger: Logger::new(
                logger.timestamps(),
                logger.log_file_path().as_deref(),
                logger.verbose(),
            )?,
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

        match self
            .hamstik
            .verify_required_commands(self.config.hamstik.assign_to_me)
        {
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

        // Advisory: Hamstik items a crashed run left claimed. Selection polls
        // only the configured candidate statuses, so an item stranded in
        // `in_progress` is invisible to the Wheel until moved back manually.
        match self
            .hamstik
            .list_assigned_to_me(&["in_progress"], &self.config.hamstik.item_types)
        {
            Ok(claimed) => {
                let orphans: Vec<_> = claimed
                    .iter()
                    .filter(|item| {
                        state
                            .current
                            .as_ref()
                            .map(|active| active.key != item.key)
                            .unwrap_or(true)
                    })
                    .collect();
                if orphans.is_empty() {
                    self.logger
                        .info("✓ No stranded in_progress items assigned to me");
                } else {
                    self.logger.warn("! Stranded in_progress item(s) assigned to me (invisible to selection until moved back to a candidate status):");
                    for item in &orphans {
                        self.logger.warn(&format!(
                            "  • {} — {} (run `hamstik work transition {} todo`)",
                            item.key, item.title, item.key
                        ));
                    }
                }
            }
            Err(e) => {
                // Advisory only: offline or permission problems must not fail
                // doctor, which is also used before first-run setup.
                self.logger.warn(&format!(
                    "! Could not check for stranded in_progress items: {e:#}"
                ));
            }
        }

        // Advisory: WIP branches left behind by skips. Drift (branch still
        // exists while its commit is reachable from main) usually means the
        // work was salvaged or superseded and the branch can be deleted.
        match self.repo.head() {
            Ok(head) => {
                let mut wip_keys: Vec<String> = state
                    .skip_ledger
                    .iter()
                    .map(|record| record.key.clone())
                    .collect();
                if let Some(active) = state.current.as_ref() {
                    wip_keys.push(active.key.clone());
                }
                wip_keys.sort();
                wip_keys.dedup();
                let mut stale = Vec::new();
                for key in &wip_keys {
                    if let Ok(branches) = self.repo.existing_wip_branches(key) {
                        for branch in branches {
                            if self.repo.is_ancestor(&branch, &head) {
                                stale.push((key.clone(), branch));
                            }
                        }
                    }
                }
                if stale.is_empty() {
                    self.logger
                        .info("✓ No stale WIP branches for known skipped items");
                } else {
                    self.logger
                        .warn("! WIP branch(es) already contained in HEAD (likely salvaged; deletion candidates):");
                    for (key, branch) in &stale {
                        self.logger.warn(&format!("  • {key}: {branch}"));
                    }
                }
            }
            Err(e) => {
                self.logger
                    .warn(&format!("! Could not check WIP branches: {e:#}"));
            }
        }

        if self.config.validation.commands.is_empty() && self.config.validation.rules.is_empty() {
            self.logger
                .warn("! No validation commands configured; final gate will rely on review only");
        } else {
            self.logger.info(&format!(
                "✓ {} base validation command(s), {} conditional rule(s) configured",
                self.config.validation.commands.len(),
                self.config.validation.rules.len()
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
        if !state.skip_ledger.is_empty() {
            self.logger.info("Skip ledger:");
            for record in &state.skip_ledger {
                self.logger.info(&format!(
                    "  - {} — `{}`{} at {}",
                    record.key,
                    record.reason,
                    record
                        .model
                        .as_deref()
                        .map(|model| format!(" (model `{model}`)"))
                        .unwrap_or_default(),
                    record.skipped_at.format("%Y-%m-%dT%H:%M:%SZ"),
                ));
                if let Some(sha) = &record.wip_sha {
                    self.logger.info(&format!(
                        "    preserved: {} ({sha})",
                        record.wip_branch.as_deref().unwrap_or("commit")
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn run(&self, max_items: Option<usize>) -> Result<()> {
        self.preflight()?;
        let limit = max_items.unwrap_or(self.config.r#loop.max_items);
        if limit == 0 {
            bail!("max-items must be greater than zero");
        }

        // Per-run counter: reset at run start so `status` shows this run's
        // completions, not an accumulation across invocations.
        let mut state = self.store.load()?;
        self.store.begin_run(&mut state)?;

        let mut completed = 0usize;
        let mut skipped = 0usize;
        let mut excluded = HashSet::new();
        while completed + skipped < limit {
            match self.process_one(&mut excluded)? {
                ProcessOutcome::Completed => {
                    completed += 1;
                    self.logger.blank();
                    self.logger
                        .info(&format!("Completed {completed}/{limit} Work Item(s)."));
                    if completed + skipped < limit {
                        self.logger
                            .info("[continue] Selecting the next eligible Work Item.");
                    }
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
        match self.process_one(&mut HashSet::new())? {
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
        self.hamstik
            .verify_required_commands(self.config.hamstik.assign_to_me)?;
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

    fn process_one(&self, excluded: &mut HashSet<String>) -> Result<ProcessOutcome> {
        let mut state = self.store.load()?;

        // Recovery first: a previous invocation may have crashed between
        // "skip requested" and "skip finished". Complete that cleanup before
        // anything else so the item is never stranded in `in_progress`.
        if state.phase == Phase::Skipping {
            let key = state.current.as_ref().unwrap().key.clone();
            self.recover_interrupted_skip(&mut state)?;
            excluded.insert(key);
            state = self.store.load()?;
        }

        if state.current.is_none() {
            if self.config.git.require_clean_start && !self.repo.is_clean()? {
                bail!("working tree is dirty; finish/stash existing work before Hamstik Wheel selects a new Work Item");
            }
            let mut candidates = self.hamstik.list_candidates(
                &self.config.hamstik.statuses,
                &self.config.hamstik.item_types,
                &self.config.hamstik.label_names,
            )?;
            candidates.retain(|item| {
                if excluded.contains(&item.key) {
                    return false;
                }
                if let Some(record) = state.last_skip(&item.key) {
                    if self.skip_is_cooling(record) {
                        self.logger.info(&format!(
                            "[select] {} cooling down after {}; considering other candidates",
                            item.key, record.reason
                        ));
                        return false;
                    }
                }
                true
            });
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
                validation_command_count: 0,
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

            // Provider availability affects every item using this model. Preserve
            // the exact phase and tree instead of throwing away completed work.
            if error.downcast_ref::<ProviderFailure>().is_some() {
                return Err(error.context("provider unavailable; active checkpoint preserved; run `hamstik-wheel resume` after restoring provider access"));
            }

            if self.config.r#loop.on_failure == config::OnFailure::Skip {
                let Some(active) = state.current.as_ref() else {
                    return Err(error);
                };
                let key = active.key.clone();
                let title = active.title.clone();
                let baseline = active.baseline_sha.clone();

                // Persist the skip intent BEFORE touching the tree. A crash
                // after this point is recovered by recover_interrupted_skip
                // on the next invocation (observed: a process kill between WIP
                // commit and baseline reset left CLI-32 claimed in
                // `in_progress` forever, invisible to selection).
                state.phase = Phase::Skipping;
                self.store.save(&mut state)?;

                if let Err(cleanup_error) = self.complete_skip_cleanup(&key, &baseline) {
                    // Recovery is designed to make this reachable only on a
                    // hard Git failure; surface both errors and stop.
                    state.last_error = Some(format!(
                        "{error_text}; skip cleanup failed: {cleanup_error:#}"
                    ));
                    let _ = self.store.save(&mut state);
                    return Err(anyhow::anyhow!(error_text)
                        .context(format!("skip cleanup failed: {cleanup_error:#}")));
                }

                self.record_skip_comment(&key, &title, &baseline, &error_text)?;
                let (wip_branch, wip_sha) = self.latest_wip(&key)?;
                let record = SkipRecord {
                    key: key.clone(),
                    title: title.clone(),
                    reason: classify_failure(&error_text),
                    model: active_model(&error_text),
                    wip_branch,
                    wip_sha,
                    skipped_at: chrono::Utc::now(),
                };
                self.store.finish_skip(&mut state, record)?;
                self.logger.info(&format!(
                    "[skip] {key} recorded; item returned to the eligible pool; continuing"
                ));
                excluded.insert(key);
                return Ok(ProcessOutcome::Skipped);
            }
            return Err(error);
        }

        Ok(ProcessOutcome::Completed)
    }

    fn skip_is_cooling(&self, record: &SkipRecord) -> bool {
        let same_model = record
            .model
            .as_deref()
            .map(|model| {
                model == self.config.models.implement || model == self.config.models.review
            })
            .unwrap_or(true);
        same_model && chrono::Utc::now() - record.skipped_at < chrono::Duration::minutes(30)
    }

    fn latest_wip(&self, key: &str) -> Result<(Option<String>, Option<String>)> {
        match self.repo.existing_wip_branches(key)?.pop() {
            Some(branch) => {
                let sha = self.repo.resolve_commit(&branch)?;
                Ok((Some(branch), Some(sha)))
            }
            None => Ok((None, None)),
        }
    }

    /// Finish a skip whose cleanup was interrupted by a crash. The persisted
    /// `Skipping` phase tells us the previous invocation had already decided
    /// to skip; complete the same idempotent cleanup (WIP preservation,
    /// baseline restore, return-to-pool transition) instead of re-implementing.
    fn recover_interrupted_skip(&self, state: &mut WheelState) -> Result<()> {
        let Some(active) = state.current.as_ref() else {
            // Inconsistent state (Skipping without an active item); reset.
            self.store.clear_active(state)?;
            return Ok(());
        };
        let key = active.key.clone();
        let baseline = active.baseline_sha.clone();
        self.logger.warn(&format!(
            "[recover] {key}: interrupted skip cleanup detected; completing it"
        ));

        if let Err(cleanup_error) = self.complete_skip_cleanup(&key, &baseline) {
            // Keep the Skipping phase so a later invocation can retry the
            // recovery; do not clear active state on a hard cleanup failure.
            state.last_error = Some(format!("skip cleanup recovery failed: {cleanup_error:#}"));
            let _ = self.store.save(state);
            return Err(anyhow::anyhow!(cleanup_error)
                .context(format!("recovery of interrupted skip for {key} failed")));
        }

        let (title, error_text) = (
            active.title.clone(),
            state
                .last_error
                .clone()
                .unwrap_or_else(|| "interrupted skip cleanup".to_string()),
        );
        self.record_skip_comment(&key, &title, &baseline, &error_text)?;
        let (wip_branch, wip_sha) = self.latest_wip(&key)?;
        let record = SkipRecord {
            key: key.clone(),
            title,
            reason: "interrupted-cleanup".to_string(),
            model: active_model(&error_text),
            wip_branch,
            wip_sha,
            skipped_at: chrono::Utc::now(),
        };
        self.store.finish_skip(state, record)?;
        self.logger.info(&format!(
            "[recover] {key}: skip completed; item returned to the eligible pool"
        ));
        Ok(())
    }

    /// Tree/branch/status half of the skip path: preserve in-progress changes
    /// on a WIP branch, restore the baseline, and return the Work Item to the
    /// eligible pool. Runs after the `Skipping` phase is persisted, so a crash
    /// anywhere inside is completed by recover_interrupted_skip.
    fn complete_skip_cleanup(&self, key: &str, baseline: &str) -> Result<()> {
        let wip_branch = self
            .repo
            .preserve_wip(key, baseline)
            .context("WIP preservation failed")?;
        if let Some(branch) = wip_branch.as_deref() {
            self.logger.info(&format!(
                "[cleanup] {key} in-progress changes preserved on branch `{branch}`"
            ));
        }
        self.repo
            .checkout_baseline(baseline)
            .context("baseline restore failed")?;
        // Return the item to the eligible pool only if a transition is needed
        // (a claim that failed before `work start` left the item untouched).
        // Idempotent under recovery: re-running is a no-op once the item is
        // back in a candidate status.
        match self.hamstik.item_status(key) {
            Ok(status) => {
                let normalized = status.trim().to_ascii_lowercase().replace(['-', ' '], "_");
                if normalized == "in_progress" || normalized == "inprogress" {
                    if let Err(transition_error) = self.hamstik.transition(key, "todo") {
                        // Non-fatal: the item stays in the pool only after a
                        // manual move, but the skip itself is already durable.
                        self.logger.warn(&format!(
                            "[skip] could not return {key} to todo ({transition_error:#}); move it back manually"
                        ));
                    } else {
                        self.logger
                            .info(&format!("[cleanup] {key} returned to `todo`"));
                    }
                }
            }
            Err(status_error) => {
                self.logger.warn(&format!(
                    "[skip] could not read {key} status before return-to-pool ({status_error:#})"
                ));
            }
        }
        Ok(())
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
            if self.config.hamstik.assign_to_me {
                self.hamstik.assign_to_me(&current.key)?;
            }
            if self.config.comments.post_started {
                let body = format!(
                    "Hamstik Wheel started automated implementation.\n\n- Baseline: `{}`\n- Implementation model: `{}`\n- Review model: `{}`",
                    current.baseline_sha, self.config.models.implement, self.config.models.review
                );
                let idem = comment_idempotency_key("start", &current.key, &current.baseline_sha);
                // Bookkeeping comments are advisory: a comment failure must
                // never fail the item (observed: IDEMPOTENCY_KEY_REUSED after
                // config changes produced a claim-time skip loop).
                if let Err(comment_error) =
                    self.hamstik.add_comment(&current.key, &body, Some(&idem))
                {
                    self.logger.warn(&format!(
                        "[claim] could not post start comment on {}: {comment_error:#}",
                        current.key
                    ));
                }
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
            let mut prompt = implementation_prompt(
                &current.key,
                &current.title,
                &current.baseline_sha,
                &context,
            );
            let (branch, sha) = match state.last_skip(&current.key) {
                Some(record) if record.wip_sha.is_some() => {
                    (record.wip_branch.clone(), record.wip_sha.clone())
                }
                _ => self.latest_wip(&current.key)?,
            };
            if let Some(sha) = sha {
                prompt.push_str(&format!("\nLatest preserved work: commit {sha}, branch {}. Inspect this exact commit read-only and salvage it; do not assume the unsuffixed WIP branch is current.\n", branch.as_deref().unwrap_or("(recorded in state)")));
            }
            prompt.push_str(&format!("\nWHEEL_VALIDATION_CONFIG (base commands always run; rules apply when changed paths match their literal prefixes):\n{}\n", serde_json::to_string_pretty(&self.config.validation)?));
            let run = self.run_implement_session(&current.key, &prompt)?;
            let result = run.result.with_context(|| {
                format!(
                    "implementation agent failed; transcript: '{}'; state preserved, run `hamstik-wheel resume` to continue {}",
                    self.log_path(&current.key, "implement.log").display(),
                    current.key
                )
            })?;
            if !result.unverified_checks.is_empty() {
                bail!(
                    "implementation blocked: required validation remains unverified: {}",
                    result.unverified_checks.join("; ")
                );
            }
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
            let (mut evidence, mut review_tag) = if state.phase == Phase::PreReviewValidation {
                self.logger.blank();
                self.logger.info("[inspect] pre-review checks");
                let commands = self.validation_commands(&current.baseline_sha)?;
                let report = run_all(
                    self.repo.root(),
                    &commands,
                    &self.logger,
                    ValidationKind::Inspection,
                )?;
                let full_evidence = report.evidence();
                let validation_log = self.log_path(&current.key, "pre-review-validation.log");
                self.write_log(&current.key, "pre-review-validation.log", &full_evidence)?;
                if report.passed {
                    self.logger.info("[inspect] pre-review checks clean");
                } else {
                    self.logger.info(&format!(
                        "[remediate] Pre-review checks found issues; the reviewer will attempt repairs (details: {}).",
                        validation_log.display()
                    ));
                }
                // Bounded digest for the prompt: full output can exceed the
                // review model's context window (observed: 49K-token overflow).
                (
                    report.evidence_digest(2000),
                    if report.passed { "review" } else { "remediate" },
                )
            } else {
                (
                    "This is a resumed review phase. Independently inspect the current working tree and rerun relevant checks; do not assume an earlier review result still applies.".to_string(),
                    "review/remediate",
                )
            };

            let starting_cycle = state.review_cycle.max(1);
            let mut passed = false;
            for cycle in starting_cycle..=self.config.r#loop.max_review_cycles {
                state.review_cycle = cycle;
                state.phase = Phase::Reviewing;
                self.store.save(state)?;
                self.logger.blank();
                self.logger.info(&format!(
                    "[{review_tag}] cycle {cycle}/{} with {}",
                    self.config.r#loop.max_review_cycles, self.config.models.review
                ));
                let mut prompt = review_prompt(
                    &current.key,
                    &current.title,
                    &current.baseline_sha,
                    &context,
                    &evidence,
                    cycle,
                );
                prompt.push_str(&format!("\nWHEEL_VALIDATION_CONFIG (re-evaluated against changed paths at final validation):\n{}\n", serde_json::to_string_pretty(&self.config.validation)?));
                let review_log = format!("review-{cycle:02}.log");
                let run = self.run_review_session(&current.key, &review_log, &prompt)?;
                let review = run.result.with_context(|| {
                    format!(
                        "review agent failed (cycle {cycle}); transcript: '{}'; state preserved, run `hamstik-wheel resume` to continue {}",
                        self.log_path(&current.key, &review_log).display(),
                        current.key
                    )
                })?;
                if !review.unverified_checks.is_empty() {
                    bail!(
                        "review blocked: required validation remains unverified: {}",
                        review.unverified_checks.join("; ")
                    );
                }
                if review.status == "blocked" {
                    bail!("review blocked: {}", review.summary);
                }
                if review.status != "pass" {
                    evidence = format!(
                        "Reviewer returned status {}. Findings:\n{}",
                        review.status,
                        review.findings.join("\n")
                    );
                    review_tag = "remediate";
                    if cycle < self.config.r#loop.max_review_cycles {
                        self.logger.info(&format!(
                            "[remediate] Review found issues; continuing with cycle {}/{}.",
                            cycle + 1,
                            self.config.r#loop.max_review_cycles
                        ));
                    }
                    continue;
                }
                if !review.findings.is_empty() {
                    evidence = format!(
                        "Reviewer claimed PASS but still reported findings:\n{}",
                        review.findings.join("\n")
                    );
                    review_tag = "remediate";
                    if cycle < self.config.r#loop.max_review_cycles {
                        self.logger.info(&format!(
                            "[remediate] Review reported unresolved findings; continuing with cycle {}/{}.",
                            cycle + 1,
                            self.config.r#loop.max_review_cycles
                        ));
                    }
                    continue;
                }

                state.phase = Phase::FinalValidation;
                self.store.save(state)?;
                self.logger.blank();
                self.logger
                    .info(&format!("[validate] final checks for cycle {cycle}"));
                let commands = self.validation_commands(&current.baseline_sha)?;
                let final_report = run_all(
                    self.repo.root(),
                    &commands,
                    &self.logger,
                    ValidationKind::Final,
                )?;
                let final_evidence = final_report.evidence();
                self.write_log(
                    &current.key,
                    &format!("final-validation-{cycle:02}.log"),
                    &final_evidence,
                )?;
                if final_report.passed {
                    state.current.as_mut().unwrap().validation_command_count = commands.len();
                    self.logger.info("[validate] Final validation passed.");
                    passed = true;
                    break;
                }
                evidence = final_report.evidence_digest(2000);
                review_tag = "remediate";
                if cycle < self.config.r#loop.max_review_cycles {
                    self.logger.info(&format!(
                        "[remediate] Final checks found issues; starting remediation cycle {}/{}.",
                        cycle + 1,
                        self.config.r#loop.max_review_cycles
                    ));
                }
            }

            if !passed {
                self.logger.error(&format!(
                    "[failed] Review and remediation did not converge after {} cycle(s).",
                    self.config.r#loop.max_review_cycles
                ));
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
            } else if self.repo.is_clean()? {
                // A clean tree at the baseline means nothing was implemented;
                // git commit would fail with "nothing to commit" (observed:
                // misparsed agent results let an unimplemented item through).
                bail!(
                    "working tree is clean at baseline {}; nothing to commit for {key}; item was not implemented",
                    current.baseline_sha,
                    key = current.key,
                );
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
                    state.current.as_ref().unwrap().validation_command_count,
                );
                let idem = comment_idempotency_key("complete", &current.key, &current.baseline_sha);
                // Advisory like the start comment: never fail a completed item
                // over a bookkeeping comment.
                if let Err(comment_error) =
                    self.hamstik.add_comment(&current.key, &body, Some(&idem))
                {
                    self.logger.warn(&format!(
                        "[complete] could not post completion comment on {}: {comment_error:#}",
                        current.key
                    ));
                }
            }
            self.logger.blank();
            self.logger
                .info(&format!("[complete] closing {}", current.key));
            self.hamstik.close(&current.key)?;

            state.completed_this_run += 1;
            // The item completed; its skip history is no longer relevant.
            if let Some(active_key) = state.current.as_ref().map(|item| item.key.clone()) {
                state.clear_skip(&active_key);
            }
            self.store.clear_active(state)?;
            match commit_sha.as_deref() {
                Some(sha) => self.logger.info(&format!(
                    "[complete] {} committed at {} and closed.",
                    current.key, sha
                )),
                None => self.logger.info(&format!(
                    "[complete] {} closed; changes left uncommitted by configuration.",
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

    fn validation_commands(&self, baseline: &str) -> Result<Vec<String>> {
        Ok(self
            .config
            .validation
            .commands_for_paths(&self.repo.changed_paths(baseline)?))
    }

    fn log_path(&self, key: &str, name: &str) -> PathBuf {
        self.logs_root.join(sanitize_component(key)).join(name)
    }

    /// Keep an immutable copy of every attempt; the original path remains a
    /// convenient latest-output alias for humans and review prompts.
    fn write_log(&self, key: &str, name: &str, content: &str) -> Result<PathBuf> {
        let path = self.log_path(key, name);
        let history = path
            .parent()
            .context("log path has no parent directory")?
            .join("history");
        fs::create_dir_all(&history)?;
        // Preserve transcripts written by older Wheel versions before replacing
        // their latest-output alias for the first time.
        if path.exists() {
            let legacy = history.join(format!("legacy-{name}"));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(legacy)
            {
                Ok(mut file) => {
                    file.write_all(&fs::read(&path)?)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.9fZ");
        for sequence in 0.. {
            let archive = history.join(format!("{stamp}-{sequence}-{name}"));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&archive)
            {
                Ok(mut file) => {
                    file.write_all(content.as_bytes())?;
                    fs::write(&path, content)?;
                    return Ok(archive);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        unreachable!()
    }

    fn agent_timeout(&self) -> Option<std::time::Duration> {
        let minutes = self.config.r#loop.agent_timeout_minutes;
        (minutes > 0).then(|| std::time::Duration::from_secs(minutes * 60))
    }

    fn run_implement_session(&self, key: &str, prompt: &str) -> Result<crate::pi::PiRun> {
        self.run_agent_session(
            key,
            "implement.log",
            &self.config.models.implement,
            prompt,
            self.config.r#loop.implement_retry,
        )
    }

    fn run_review_session(&self, key: &str, log: &str, prompt: &str) -> Result<crate::pi::PiRun> {
        self.run_agent_session(key, log, &self.config.models.review, prompt, true)
    }

    fn run_agent_session(
        &self,
        key: &str,
        log: &str,
        model: &str,
        prompt: &str,
        retry_process: bool,
    ) -> Result<crate::pi::PiRun> {
        let mut provider_retries = 0;
        let mut process_retried = false;
        loop {
            let mut run = match self
                .pi
                .run(model, prompt, self.agent_timeout(), &self.logger)
            {
                Ok(run) => run,
                Err(error) => crate::pi::PiRun {
                    transcript:
                        serde_json::json!({"type": "wheel_error", "error": format!("{error:#}")})
                            .to_string(),
                    result: Err(error),
                },
            };
            let path = self.write_log(key, log, &run.transcript)?;
            self.logger
                .info(&format!("[agent] {key} transcript: {}", path.display()));
            match run.result {
                Ok(_) => return Ok(run),
                Err(error) => {
                    let retry = if let Some(provider) = error.downcast_ref::<ProviderFailure>() {
                        if provider.retryable
                            && provider_retries < self.config.r#loop.provider_retries
                        {
                            let delay = self
                                .config
                                .r#loop
                                .provider_retry_delay_seconds
                                .saturating_mul(1u64 << provider_retries)
                                .min(300);
                            provider_retries += 1;
                            self.logger.warn(&format!("[retry] {key}: {error:#}; provider retry {provider_retries}/{} in {delay}s", self.config.r#loop.provider_retries));
                            std::thread::sleep(std::time::Duration::from_secs(delay));
                            true
                        } else {
                            false
                        }
                    } else if retry_process
                        && !process_retried
                        && is_transient_agent_failure(&format!("{error:#}"))
                    {
                        process_retried = true;
                        self.logger
                            .warn(&format!("[retry] {key}: {error:#}; retrying agent once"));
                        true
                    } else {
                        false
                    };
                    if !retry {
                        run.result = Err(error.context(format!(
                            "agent session failed; transcript: {}",
                            path.display()
                        )));
                        return Ok(run);
                    }
                }
            }
        }
    }

    /// Record a failure comment on the Work Item so the run's history shows
    /// what happened overnight. Advisory: a comment failure must never fail
    /// the skip itself.
    fn record_skip_comment(
        &self,
        key: &str,
        title: &str,
        baseline: &str,
        error: &str,
    ) -> Result<()> {
        self.logger.warn(&format!("[skip] {key}: {error}"));
        let body = format!(
            "Hamstik Wheel could not complete automated work for this Work Item and moved on (on_failure = skip).\n\n- Failure: {error}\n- Baseline: `{baseline}`\n- The item was returned to `todo` and stays eligible for later runs"
        );
        let idem = comment_idempotency_key("skip", key, baseline);
        if let Err(comment_error) = self.hamstik.add_comment(key, &body, Some(&idem)) {
            self.logger.warn(&format!(
                "[skip] could not post failure comment on {title}: {comment_error:#}"
            ));
        }
        Ok(())
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

/// Idempotency key for one specific comment-posting attempt. Includes the
/// current UTC time so each attempt is its own request; a key shared across
/// attempts whose bodies differ makes the server reject the retry with
/// IDEMPOTENCY_KEY_REUSED (observed after changing the review model).
fn comment_idempotency_key(kind: &str, key: &str, baseline: &str) -> String {
    let short = baseline.chars().take(12).collect::<String>();
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    format!(
        "hamstik-wheel-{kind}-{}-{short}-{stamp}",
        sanitize_component(key)
    )
}

/// Whether an agent-session failure is transient (worth one retry) versus a
/// deterministic answer. Wall-clock timeouts, abnormal process exits, and
/// unparsable output are transient; everything the caller classified as an
/// explicit `blocked` result never reaches this function (it is a successful
/// parse, not an Err).
fn is_transient_agent_failure(error_text: &str) -> bool {
    const TRANSIENT: &[&str] = &[
        "exceeded the session timeout",
        "exited with status",
        "HAMSTIK_WHEEL_RESULT",
        "failed waiting for Pi",
        "failed to start Pi",
        "failed writing prompt to Pi stdin",
    ];
    TRANSIENT.iter().any(|needle| error_text.contains(needle))
}

/// Compact failure classification for the skip ledger. Deliberately coarse:
/// the ledger drives re-selection backoff, not diagnosis (the item's skip
/// comment and logs carry the details).
fn classify_failure(error_text: &str) -> String {
    const RULES: &[(&str, &str)] = &[
        ("rate-limited", "provider-rate-limit"),
        ("request failed (resolved model", "provider-error"),
        ("exceeded the session timeout", "model-timeout"),
        ("HAMSTIK_WHEEL_RESULT", "no-result-marker"),
        ("git commit failed", "git-commit"),
        ("git add", "git-add"),
        ("git reset --hard", "git-reset"),
        ("git clean", "git-clean"),
        ("implementation blocked", "implement-blocked"),
        ("review blocked", "review-blocked"),
        ("did not reach PASS", "review-not-converged"),
        ("implementation agent failed", "implement-failed"),
        ("review agent failed", "review-failed"),
        ("validation", "validation-failed"),
        ("could not read", "hamstik-api"),
        ("hamstik", "hamstik-api"),
    ];
    let lowered = error_text.to_ascii_lowercase();
    for (needle, reason) in RULES {
        if lowered.contains(&needle.to_ascii_lowercase()) {
            return (*reason).to_string();
        }
    }
    "other".to_string()
}

/// Extract the model identifier named in a failure message, when present.
/// Timeout/exit errors from Pi always embed the model id
/// (`Pi model <id> exceeded the session timeout...`), which is what the
/// re-selection backoff compares against the current configuration.
fn active_model(error_text: &str) -> Option<String> {
    let marker = "Pi model ";
    let start = error_text.find(marker)? + marker.len();
    let rest = &error_text[start..];
    let end = rest.find(char::is_whitespace)?;
    let model = &rest[..end];
    (!model.is_empty()).then(|| model.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_failure_maps_known_shapes() {
        assert_eq!(
            classify_failure(
                "review agent failed (cycle 1); transcript: 'x': Pi model gb10/bonsai2-27b exceeded the session timeout of 6300s: session terminated after exceeding the wall-clock limit"
            ),
            "model-timeout"
        );
        assert_eq!(classify_failure("git commit failed: "), "git-commit");
        assert_eq!(
            classify_failure("implementation agent failed (after one retry)"),
            "implement-failed"
        );
        assert_eq!(
            classify_failure("review/validation did not reach PASS within 3 cycle(s)"),
            "review-not-converged"
        );
        assert_eq!(classify_failure("something entirely novel"), "other");
    }

    #[test]
    fn classify_failure_prefers_timeout_over_agent_failure() {
        // The timeout marker is the actionable part; ordering in RULES must
        // keep model-timeout ahead of the generic agent-failed reasons.
        let text = "review agent failed: Pi model m1 exceeded the session timeout of 60s";
        assert_eq!(classify_failure(text), "model-timeout");
    }

    #[test]
    fn active_model_extracts_model_ids() {
        assert_eq!(
            active_model(
                "review agent failed (cycle 1): Pi model gb10/bonsai2-27b exceeded the session timeout of 6300s: boom"
            ),
            Some("gb10/bonsai2-27b".to_string())
        );
        assert_eq!(
            active_model("Pi model openrouter/deepseek-v4.1-flash exited with status Some(1)"),
            Some("openrouter/deepseek-v4.1-flash".to_string())
        );
        assert_eq!(active_model("git commit failed: "), None);
    }

    #[test]
    fn comment_idempotency_key_differs_per_attempt() {
        let a = comment_idempotency_key("skip", "CLI-32", "e56b1eb7728403c4");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let b = comment_idempotency_key("skip", "CLI-32", "e56b1eb7728403c4");
        assert_ne!(a, b, "each attempt must own its idempotency key");
        assert!(a.starts_with("hamstik-wheel-skip-CLI-32-e56b1eb77284-"));
    }
}
