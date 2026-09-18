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
            bail!(
                "git commit failed: {}",
                String::from_utf8_lossy(&commit.stderr).trim()
            );
        }
        self.head()
    }

    pub fn expand_commit_message(template: &str, key: &str, title: &str) -> String {
        template.replace("{key}", key).replace("{title}", title)
    }
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
}
