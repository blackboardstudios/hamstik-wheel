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

    fn run(&self, args: &[&str]) -> Result<String> {
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
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    pub fn head(&self) -> Result<String> {
        self.run(&["rev-parse", "HEAD"])
    }

    pub fn is_clean(&self) -> Result<bool> {
        Ok(self.run(&["status", "--porcelain"])?.is_empty())
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
            // an empty message because the explanation was on stdout).
            let stderr = String::from_utf8_lossy(&commit.stderr).into_owned();
            let stdout = String::from_utf8_lossy(&commit.stdout).into_owned();
            let stderr = stderr.trim();
            let stdout = stdout.trim();
            let detail = if stderr.is_empty() {
                stdout.to_string()
            } else if stdout.is_empty() {
                stderr.to_string()
            } else {
                format!("{stderr}; stdout: {stdout}")
            };
            bail!("git commit failed: {detail}");
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
    /// Returns the branch name, or Ok(None) when the tree was already clean
    /// (nothing to preserve). Never alters the current branch's history.
    pub fn preserve_wip(&self, key: &str, _baseline_sha: &str) -> Result<Option<String>> {
        if self.is_clean()? {
            return Ok(None);
        }
        // A branch from an earlier skip of the same key may already exist
        // (observed: a timed-out review after a prior skip left
        // `wheel/wip/CLI-32` in place); append a numeric suffix instead of
        // failing the preservation run.
        let base = sanitize_ref_component(key);
        let mut branch = format!("wheel/wip/{base}");
        let mut suffix = 1usize;
        while self.ref_exists(&branch) {
            suffix += 1;
            branch = format!("wheel/wip/{base}-{suffix}");
        }

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
            let detail = if stderr.is_empty() {
                stdout.to_string()
            } else if stdout.is_empty() {
                stderr.to_string()
            } else {
                format!("{stderr}; stdout: {stdout}")
            };
            bail!("git commit failed during WIP preservation: {detail}");
        }
        let wip_sha = self.head()?;

        // Anchor a branch at the WIP commit (its parent chain already
        // includes the baseline), then reset the working tree/HEAD back to
        // the baseline so the next selection starts clean.
        let branch_at = Command::new("git")
            .current_dir(&self.root)
            .args(["branch", branch.as_str(), wip_sha.as_str()])
            .output()
            .with_context(|| format!("failed to create WIP branch {branch}"))?;
        if !branch_at.status.success() {
            bail!(
                "git branch {branch} failed: {}",
                String::from_utf8_lossy(&branch_at.stderr).trim()
            );
        }

        self.checkout_baseline(_baseline_sha)?;
        Ok(Some(branch))
    }

    /// Whether a Git ref (branch or tag) with this exact name exists.
    fn ref_exists(&self, ref_name: &str) -> bool {
        Command::new("git")
            .current_dir(&self.root)
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{ref_name}^{{commit}}"),
            ])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
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
}
