// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone)]
pub struct GitRepo {
    root: PathBuf,
}

impl GitRepo {
    /// Construct a repo handle over an explicit root (test convenience).
    #[allow(dead_code)]
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    pub fn discover() -> Result<Self> {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .context("failed to execute git; is it installed?")?;
        if !output.status.success() {
            bail!(
                "current directory is not inside a Git repository: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let root = PathBuf::from(String::from_utf8(output.stdout)?.trim());
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn run_raw(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .current_dir(&self.root)
            .args(args)
            .output()
            .with_context(|| format!("failed to execute git {}", args.join(" ")))?;
        if !output.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8(output.stdout)?)
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        Ok(self.run_raw(args)?.trim().to_string())
    }

    pub fn head(&self) -> Result<String> {
        self.run(&["rev-parse", "HEAD"])
    }

    pub fn resolve_commit(&self, reference: &str) -> Result<String> {
        self.run(&["rev-parse", "--verify", &format!("{reference}^{{commit}}")])
    }

    /// Includes staged, unstaged, deleted, renamed, and new files. Disable
    /// rename detection so both sides of a move activate validation rules.
    pub fn changed_paths(&self, baseline: &str) -> Result<Vec<String>> {
        let mut paths = Vec::new();
        for args in [
            vec!["diff", "--name-only", "--no-renames", "-z", baseline, "--"],
            vec!["ls-files", "--others", "--exclude-standard", "-z"],
        ] {
            let output = self.run_raw(&args)?;
            paths.extend(
                output
                    .split('\0')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            );
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    pub fn is_clean(&self) -> Result<bool> {
        Ok(self.run(&["status", "--porcelain"])?.is_empty())
    }

    /// Whether `ancestor` (a branch or SHA) is reachable from `descendant`.
    /// Used to detect WIP branches whose content was already merged/salvaged.
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        Command::new("git")
            .current_dir(&self.root)
            .args(["merge-base", "--is-ancestor", ancestor, descendant])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    pub fn metadata_path(&self, relative: &str) -> Result<PathBuf> {
        let rendered = self.run(&["rev-parse", "--git-path", relative])?;
        let path = PathBuf::from(rendered);
        Ok(if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        })
    }

    pub fn commit_all(&self, message: &str) -> Result<String> {
        let add = Command::new("git")
            .current_dir(&self.root)
            .args(["add", "-A"])
            .status()
            .context("failed to execute git add -A")?;
        if !add.success() {
            bail!("git add -A failed");
        }

        let commit = Command::new("git")
            .current_dir(&self.root)
            .args(["commit", "-m", message])
            .output()
            .context("failed to execute git commit")?;
        if !commit.status.success() {
            // git writes "nothing to commit, working tree clean" and other
            // commit failures to stdout, so include both streams (observed:
            // an empty message because the explanation was on stdout). The
            // exit code is always included: an empty detail with no code
            // (observed: a hook exit with no output) is undiagnosable.
            let stderr = String::from_utf8_lossy(&commit.stderr).into_owned();
            let stdout = String::from_utf8_lossy(&commit.stdout).into_owned();
            let stderr = stderr.trim();
            let stdout = stdout.trim();
            let detail = match (stderr.is_empty(), stdout.is_empty()) {
                (true, true) => format!("exit {:?}", commit.status.code()),
                (false, true) => stderr.to_string(),
                (true, false) => stdout.to_string(),
                (false, false) => format!("{stderr}; stdout: {stdout}"),
            };
            bail!(
                "git commit failed (exit {:?}): {detail}",
                commit.status.code()
            );
        }
        self.head()
    }

    /// Restore the working tree to a baseline commit: tracked files are
    /// reset to the baseline and untracked files/directories (agent-created
    /// artifacts) are removed. Used only on the skip path for tree restore
    /// after a failed item whose changes should not be kept.
    pub fn checkout_baseline(&self, baseline_sha: &str) -> Result<()> {
        let reset = Command::new("git")
            .current_dir(&self.root)
            .args(["reset", "--hard", baseline_sha])
            .output()
            .with_context(|| format!("failed to execute git reset --hard {baseline_sha}"))?;
        if !reset.status.success() {
            bail!(
                "git reset --hard {} failed: {}",
                baseline_sha,
                String::from_utf8_lossy(&reset.stderr).trim()
            );
        }

        let clean = Command::new("git")
            .current_dir(&self.root)
            .args(["clean", "-fd"])
            .output()
            .with_context(|| "failed to execute git clean -fd")?;
        if !clean.status.success() {
            bail!(
                "git clean -fd failed: {}",
                String::from_utf8_lossy(&clean.stderr).trim()
            );
        }
        Ok(())
    }

    pub fn expand_commit_message(template: &str, key: &str, title: &str) -> String {
        template.replace("{key}", key).replace("{title}", title)
    }

    /// Preserve in-progress working-tree changes from a failed Work Item on
    /// a dedicated WIP branch before the tree is restored to the baseline.
    /// Returns the branch name, or Ok(None) when there was nothing to
    /// preserve (clean tree at the baseline). Never alters the current
    /// branch's history.
    ///
    /// Also heals an interrupted preservation (observed: a process kill
    /// between the WIP commit and the branch anchor left CLI-32's work as an
    /// unanchored commit ahead of the baseline with a clean tree — a naive
    /// "clean tree means nothing to do" check would then destroy it on the
    /// baseline reset). A clean tree whose HEAD is ahead of the baseline is
    /// treated as an unanchored WIP commit: it is anchored on the branch
    /// before the reset, so preservation never loses content.
    pub fn preserve_wip(&self, key: &str, baseline_sha: &str) -> Result<Option<String>> {
        // Standard path: commit the dirty tree (its parent chain already
        // includes whatever HEAD was, which on the skip path is the baseline).
        if !self.is_clean()? {
            let add = Command::new("git")
                .current_dir(&self.root)
                .args(["add", "-A"])
                .status()
                .context("failed to execute git add -A for WIP preservation")?;
            if !add.success() {
                bail!("git add -A failed during WIP preservation");
            }

            let commit = Command::new("git")
                .current_dir(&self.root)
                .args([
                    "commit",
                    "-m",
                    &format!("WIP: {key} automated work (preserved before baseline restore)"),
                ])
                .output()
                .context("failed to execute git commit for WIP preservation")?;
            if !commit.status.success() {
                let stderr = String::from_utf8_lossy(&commit.stderr).into_owned();
                let stdout = String::from_utf8_lossy(&commit.stdout).into_owned();
                let stderr = stderr.trim();
                let stdout = stdout.trim();
                let detail = match (stderr.is_empty(), stdout.is_empty()) {
                    (true, true) => format!("exit {:?}", commit.status.code()),
                    (false, true) => stderr.to_string(),
                    (true, false) => stdout.to_string(),
                    (false, false) => format!("{stderr}; stdout: {stdout}"),
                };
                bail!(
                    "git commit failed during WIP preservation (exit {:?}): {detail}",
                    commit.status.code()
                );
            }
        }

        // Whether HEAD holds a WIP commit worth anchoring: either we just
        // committed the dirty tree, or a previous crashed preservation left
        // HEAD ahead of the baseline with a clean tree. A repository with no
        // commits (or an unreadable HEAD) has nothing to anchor.
        let wip_sha = match self.head() {
            Ok(sha) => sha,
            Err(_) => return Ok(None),
        };
        if wip_sha == baseline_sha {
            return Ok(None);
        }

        // Anchor a branch at the WIP commit, then reset the working
        // tree/HEAD back to the baseline so the next selection starts clean.
        // A branch from an earlier skip of the same key may already exist
        // (observed: a timed-out review after a prior skip left
        // `wheel/wip/CLI-32` in place); if it already points at this exact
        // commit the anchor is done, otherwise append a numeric suffix
        // instead of failing the preservation run.
        let base = format!("wheel/wip/{}", sanitize_ref_component(key));
        let existing = self.existing_wip_branches(key)?;
        let branch = if let Some(branch) = existing
            .iter()
            .find(|branch| self.branch_points_at(branch, &wip_sha))
        {
            branch.clone()
        } else {
            match existing.last() {
                None => base,
                Some(last) => {
                    let suffix = last
                        .strip_prefix(&format!("{base}-"))
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(1);
                    format!(
                        "{base}-{}",
                        suffix
                            .checked_add(1)
                            .context("WIP branch suffix overflow")?
                    )
                }
            }
        };

        let branch_at = Command::new("git")
            .current_dir(&self.root)
            .args(["branch", branch.as_str(), wip_sha.as_str()])
            .output()
            .with_context(|| format!("failed to create WIP branch {branch}"))?;
        if !branch_at.status.success() && !self.branch_points_at(&branch, &wip_sha) {
            bail!(
                "git branch {branch} failed: {}",
                String::from_utf8_lossy(&branch_at.stderr).trim()
            );
        }

        self.checkout_baseline(baseline_sha)?;
        Ok(Some(branch))
    }

    /// Whether a branch exists and points exactly at `sha`.
    fn branch_points_at(&self, branch: &str, sha: &str) -> bool {
        Command::new("git")
            .current_dir(&self.root)
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{branch}^{{commit}}"),
            ])
            .output()
            .map(|output| {
                output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == sha
            })
            .unwrap_or(false)
    }

    /// Name of the WIP branch `preserve_wip` would create for a key, plus any
    /// numeric-suffixed variants that already exist. Used by crash recovery
    /// (was preservation interrupted?) and by doctor advisories. Enumeration
    /// tolerates deleted suffixes and sorts by creation sequence.
    pub fn existing_wip_branches(&self, key: &str) -> Result<Vec<String>> {
        let base = format!("wheel/wip/{}", sanitize_ref_component(key));
        let prefix = format!("{base}-");
        let output = self.run(&[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads/wheel/wip/",
        ])?;
        let mut branches: Vec<_> = output
            .lines()
            .filter_map(|name| {
                let suffix = if name == base {
                    1
                } else {
                    name.strip_prefix(&prefix)?.parse::<usize>().ok()?
                };
                Some((suffix, name.to_string()))
            })
            .collect();
        branches.sort_by_key(|(suffix, _)| *suffix);
        Ok(branches.into_iter().map(|(_, name)| name).collect())
    }
}

fn sanitize_ref_component(value: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_commit_template() {
        assert_eq!(
            GitRepo::expand_commit_message("{key}: {title}", "HAM-42", "Do a thing"),
            "HAM-42: Do a thing"
        );
    }

    #[test]
    fn changed_paths_include_new_deleted_staged_and_renamed_files() {
        let dir = tempfile::tempdir().unwrap();
        let repo = GitRepo::new(dir.path()).unwrap();
        repo.run(&["init", "-q"]).unwrap();
        repo.run(&["config", "user.email", "wheel@test"]).unwrap();
        repo.run(&["config", "user.name", "wheel"]).unwrap();
        repo.run(&["config", "commit.gpgsign", "false"]).unwrap();
        for name in ["delete.sql", "rename.sql", "edit.sql"] {
            std::fs::write(dir.path().join(name), "original\n").unwrap();
        }
        repo.run(&["add", "-A"]).unwrap();
        repo.run(&["commit", "-qm", "base"]).unwrap();
        let baseline = repo.head().unwrap();
        std::fs::remove_file(dir.path().join("delete.sql")).unwrap();
        repo.run(&["mv", "rename.sql", "renamed.sql"]).unwrap();
        std::fs::write(dir.path().join("edit.sql"), "changed\n").unwrap();
        std::fs::write(dir.path().join(" leading space.sql"), "new\n").unwrap();
        assert_eq!(
            repo.changed_paths(&baseline).unwrap(),
            vec![
                " leading space.sql",
                "delete.sql",
                "edit.sql",
                "rename.sql",
                "renamed.sql"
            ]
        );
    }

    #[test]
    fn sanitize_ref_component_replaces_invalid_characters() {
        assert_eq!(sanitize_ref_component("CLI-52"), "CLI-52");
        assert_eq!(sanitize_ref_component("HAM 42/x"), "HAM_42_x");
    }

    #[test]
    fn preserve_wip_saves_changes_on_branch_and_restores_tree() {
        // Build a scratch repo with one commit.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .current_dir(root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "wheel@test"]);
        git(&["config", "user.name", "wheel"]);
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        let baseline = {
            let out = Command::new("git")
                .current_dir(root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        let repo = GitRepo::new(root).unwrap();
        // Dirty the tree: modify a tracked file, add an untracked file.
        std::fs::write(root.join("base.txt"), "changed\n").unwrap();
        std::fs::write(root.join("extra.txt"), "new\n").unwrap();

        let branch = repo.preserve_wip("CLI-63", &baseline).unwrap().unwrap();
        assert_eq!(branch, "wheel/wip/CLI-63");
        assert!(repo.is_clean().unwrap());

        // The WIP branch must contain the changes; the working tree must not.
        let show = Command::new("git")
            .current_dir(root)
            .args(["show", &format!("{branch}:base.txt")])
            .output()
            .unwrap();
        assert!(show.status.success());
        let branch_content = String::from_utf8_lossy(&show.stdout).into_owned();
        assert_eq!(branch_content, "changed\n");
        assert_eq!(
            std::fs::read_to_string(root.join("base.txt")).unwrap(),
            "base\n"
        );
        assert!(!root.join("extra.txt").exists());
    }

    #[test]
    fn preserve_wip_on_clean_tree_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let out = Command::new("git")
            .current_dir(root)
            .args(["init", "-q"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let repo = GitRepo::new(root).unwrap();
        assert!(repo.preserve_wip("CLI-1", "HEAD").unwrap().is_none());
    }

    #[test]
    fn existing_wip_branches_reports_suffixed_variants() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .current_dir(root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "wheel@test"]);
        git(&["config", "user.name", "wheel"]);
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        let repo = GitRepo::new(root).unwrap();

        assert!(repo.existing_wip_branches("CLI-1").unwrap().is_empty());

        // Simulate two prior skips of the same key; each skip requires a
        // dirty tree for preserve_wip to act.
        std::fs::write(root.join("first.txt"), "first\n").unwrap();
        let first = repo.preserve_wip("CLI-7", "HEAD").unwrap().unwrap();
        assert_eq!(first, "wheel/wip/CLI-7");
        std::fs::write(root.join("more.txt"), "more\n").unwrap();
        let second = repo.preserve_wip("CLI-7", "HEAD").unwrap().unwrap();
        assert_eq!(second, "wheel/wip/CLI-7-2");

        let branches = repo.existing_wip_branches("CLI-7").unwrap();
        assert_eq!(
            branches,
            vec![
                "wheel/wip/CLI-7".to_string(),
                "wheel/wip/CLI-7-2".to_string()
            ]
        );
        // A different key has no branches.
        assert!(repo.existing_wip_branches("CLI-8").unwrap().is_empty());
    }

    /// The observed CLI-32 crash: preserve_wip committed the tree (commit
    /// landed on the current branch), but the process died before the
    /// baseline reset and before the WIP branch was anchored. Recovery must
    /// reach a clean tree at the baseline without duplicating the preserved
    /// content on a second WIP commit.
    #[test]
    fn recovery_after_interrupted_preservation_reaches_clean_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .current_dir(root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "wheel@test"]);
        git(&["config", "user.name", "wheel"]);
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        let baseline = {
            let out = Command::new("git")
                .current_dir(root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        let repo = GitRepo::new(root).unwrap();

        // Phase 1: normal preservation completes (this is what happened on
        // the first skip of CLI-32).
        std::fs::write(root.join("base.txt"), "changed\n").unwrap();
        let branch = repo.preserve_wip("CLI-32", &baseline).unwrap().unwrap();
        assert_eq!(branch, "wheel/wip/CLI-32");
        assert!(repo.is_clean().unwrap());
        assert_eq!(repo.head().unwrap(), baseline);

        // Phase 2: simulate the interrupted attempt — dirty the tree again,
        // commit it manually (as preserve_wip does) but crash before the
        // branch anchor and baseline reset. HEAD is now ahead of baseline
        // with a WIP-looking commit and NO wheel/wip branch for it.
        std::fs::write(root.join("partial.txt"), "partial\n").unwrap();
        git(&["add", "-A"]);
        git(&[
            "commit",
            "-qm",
            "WIP: CLI-32 automated work (preserved before baseline restore)",
        ]);
        let interrupted_head = repo.head().unwrap();
        assert_ne!(interrupted_head, baseline);

        // Recovery: preserve_wip sees a clean tree but HEAD ahead of the
        // baseline — the interrupted WIP commit. It anchors that commit on a
        // fresh suffixed branch (attempt 1's branch already exists at its
        // own commit) and restores the baseline, never destroying content.
        let recovered_branch = repo.preserve_wip("CLI-32", &baseline).unwrap();
        assert_eq!(
            recovered_branch.as_deref(),
            Some("wheel/wip/CLI-32-2"),
            "the interrupted commit must be anchored on a new suffixed branch, not lost"
        );
        repo.checkout_baseline(&baseline).unwrap();
        assert!(repo.is_clean().unwrap());
        assert_eq!(repo.head().unwrap(), baseline);
        assert!(!root.join("partial.txt").exists());

        // The interrupted commit content survives on the anchored branch.
        let anchored = recovered_branch.unwrap();
        let show = Command::new("git")
            .current_dir(root)
            .args(["show", &format!("{anchored}:partial.txt")])
            .output()
            .unwrap();
        assert!(show.status.success());
        assert_eq!(String::from_utf8_lossy(&show.stdout), "partial\n");

        // Second recovery pass is a no-op: nothing dirty, nothing new.
        assert!(repo.preserve_wip("CLI-32", &baseline).unwrap().is_none());
        repo.checkout_baseline(&baseline).unwrap();
        assert_eq!(repo.head().unwrap(), baseline);
    }

    #[test]
    fn is_ancestor_detects_contained_branches() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .current_dir(root)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {} failed", args.join(" "));
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "wheel@test"]);
        git(&["config", "user.name", "wheel"]);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        let base = {
            let out = Command::new("git")
                .current_dir(root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["branch", "topic"]);
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "advance"]);
        let head = {
            let out = Command::new("git")
                .current_dir(root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let repo = GitRepo::new(root).unwrap();
        // topic (at base) is an ancestor of the advanced HEAD; HEAD is not an
        // ancestor of itself-as-branch-check direction matters.
        assert!(repo.is_ancestor("topic", &head));
        assert!(!repo.is_ancestor(&head, "topic"));
        assert!(repo.is_ancestor(&base, &base));
        assert!(!repo.is_ancestor("nonexistent-ref", &head));
    }
}
