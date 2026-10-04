// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0
#![cfg(target_os = "linux")]

#[test]
fn real_tool_sandbox_regressions() {
    let output = std::process::Command::new("node")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["--test", "tests/sandbox.test.mjs"])
        .output()
        .expect("Linux tests require Node.js and /usr/bin/bwrap (bubblewrap)");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
