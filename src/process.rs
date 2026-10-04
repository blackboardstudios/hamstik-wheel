// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

use anyhow::{bail, Context, Result};
use std::{
    process::{Child, Command},
    time::{Duration, Instant},
};

/// Isolate the process group, and on Linux tie the child lifetime to Wheel.
pub fn configure(command: &mut Command) {
    #[cfg(not(unix))]
    let _ = command;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        #[cfg(target_os = "linux")]
        {
            let parent = std::process::id();
            // SAFETY: the post-fork closure uses only async-signal-safe libc
            // syscalls, no allocation or locks. Verify the parent to close the
            // fork/prctl race. Constants are Linux PR_SET_PDEATHSIG/SIGKILL.
            unsafe {
                command.pre_exec(move || {
                    unsafe extern "C" {
                        fn prctl(option: i32, ...) -> i32;
                        fn getppid() -> i32;
                    }
                    if prctl(
                        1,
                        9 as std::os::raw::c_ulong,
                        0 as std::os::raw::c_ulong,
                        0 as std::os::raw::c_ulong,
                        0 as std::os::raw::c_ulong,
                    ) != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    if getppid() as u32 != parent {
                        return Err(std::io::Error::from_raw_os_error(10));
                    }
                    Ok(())
                });
            }
        }
    }
}

pub struct ManagedChild(pub Child);
impl ManagedChild {
    pub fn wait(&mut self, timeout: Option<Duration>) -> Result<std::process::ExitStatus> {
        let start = Instant::now();
        loop {
            if let Some(status) = self.0.try_wait().context("failed polling agent process")? {
                // Descendants must not outlive a successful session either.
                self.terminate_group();
                return Ok(status);
            }
            if timeout.is_some_and(|limit| start.elapsed() >= limit) {
                self.terminate_group();
                let _ = self.0.wait();
                bail!("session terminated after exceeding the wall-clock limit");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    fn terminate_group(&mut self) {
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // SAFETY: this group was created specifically for this child.
            unsafe {
                kill(-(self.0.id() as i32), 9);
            }
        }
        let _ = self.0.kill();
    }
}
impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.terminate_group();
        let _ = self.0.wait();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Stdio;
    #[test]
    fn timeout_kills_descendants_holding_stdout_open() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 60 & wait"])
            .stdout(Stdio::piped());
        configure(&mut command);
        let mut child = ManagedChild(command.spawn().unwrap());
        let mut pipe = child.0.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            use std::io::Read;
            let mut s = String::new();
            pipe.read_to_string(&mut s).unwrap();
        });
        assert!(child.wait(Some(Duration::from_millis(100))).is_err());
        reader.join().unwrap();
    }
}

/// Capture a managed subprocess without letting inherited pipes bypass its
/// deadline. Used by validation so killing Wheel also tears down its runner.
pub fn output(command: &mut Command, timeout: Duration) -> Result<std::process::Output> {
    use std::{io::Read, process::Stdio};
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure(command);
    let mut child = ManagedChild(command.spawn()?);
    let mut stdout = child.0.stdout.take().unwrap();
    let mut stderr = child.0.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let status = child.wait(Some(timeout));
    let stdout = out
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
    let stderr = err
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
    Ok(std::process::Output {
        status: status?,
        stdout,
        stderr,
    })
}
