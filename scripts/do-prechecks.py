#!/usr/bin/env python3
"""Run the Hamstik Wheel's local quality gates in fail-fast order.

Run this before committing to main or opening a pull request so every gate
the project relies on has already passed locally. The checks mirror the
repository's canonical quality gates plus Git diff hygiene:

    cargo fmt --all --check
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
    cargo test --locked --workspace
    cargo build --locked --workspace --release

Examples:
    ./scripts/do-prechecks.py
    ./scripts/do-prechecks.py --skip-build
    ./scripts/do-prechecks.py --only format,clippy
    ./scripts/do-prechecks.py --only whitespace
"""

from __future__ import annotations

import argparse
import os
import re
import shlex
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence

try:
    from rich import box
    from rich.console import Console
    from rich.markup import escape
    from rich.panel import Panel
    from rich.table import Table
    from rich.text import Text
except ModuleNotFoundError:
    print(
        "Rich is required for this precheck utility. Install it for the active "
        "interpreter with: python3 -m pip install rich",
        file=sys.stderr,
    )
    raise SystemExit(2)


REPO_ROOT = Path(__file__).resolve().parents[1]

ANSI_ESCAPE_PATTERN = re.compile(
    r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))"
)
COMPILER_WARNING_PATTERN = re.compile(
    r"\bwarning(?:s)?:\s|\bwarning: unused\b|(?:^|\s)⚠(?:\s|$)",
    re.IGNORECASE,
)

console = Console(highlight=False)


@dataclass(frozen=True)
class Check:
    """One fail-fast quality gate."""

    key: str
    name: str
    command: tuple[str, ...]
    detail: str
    reject_warnings: bool = False
    require_files: tuple[str, ...] = ()
    require_tools: tuple[str, ...] = ()


@dataclass(frozen=True)
class CheckResult:
    """The result of running one quality gate."""

    check: Check
    returncode: int
    elapsed_seconds: float
    warning_lines: tuple[str, ...] = ()

    @property
    def passed(self) -> bool:
        return self.returncode == 0 and not self.warning_lines


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Run the repository's quality gates in fail-fast order: format, "
            "lint, tests, and release build. Stop immediately when a check "
            "fails. Use this before committing to main or opening a PR."
        )
    )
    parser.add_argument(
        "--skip-build",
        action="store_true",
        help=(
            "Skip the release build (useful for quick edit-verify loops; "
            "run the full suite before committing to main or opening a PR)."
        ),
    )
    parser.add_argument(
        "--only",
        default=None,
        help=(
            "Comma-separated subset of checks to run "
            "(format, clippy, tests, release, whitespace)."
        ),
    )
    return parser.parse_args()


def checks_for(*, skip_build: bool, only: str | None) -> list[Check]:
    """Return checks ordered from quick feedback to longest-running work."""

    checks = [
        Check(
            key="format",
            name="Format",
            command=("cargo", "fmt", "--all", "--check"),
            detail="Verify every Rust file matches rustfmt's canonical formatting.",
            require_files=("Cargo.toml",),
            require_tools=("cargo",),
        ),
        Check(
            key="clippy",
            name="Clippy",
            command=(
                "cargo",
                "clippy",
                "--locked",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ),
            detail=(
                "Lint all workspace targets and features with warnings denied "
                "without allowing Cargo.lock to change."
            ),
            require_files=("Cargo.toml", "Cargo.lock"),
            require_tools=("cargo",),
        ),
        Check(
            key="tests",
            name="Tests",
            command=("cargo", "test", "--locked", "--workspace"),
            detail=(
                "Run the complete workspace unit, doc, and integration test suites "
                "without allowing Cargo.lock to change."
            ),
            require_files=("Cargo.toml", "Cargo.lock"),
            require_tools=("cargo",),
        ),
        Check(
            key="release",
            name="Release build",
            command=("cargo", "build", "--locked", "--workspace", "--release"),
            detail=(
                "Build the optimized workspace release artifacts with the lockfile "
                "held fixed and reject compiler warning output."
            ),
            reject_warnings=True,
            require_files=("Cargo.toml", "Cargo.lock"),
            require_tools=("cargo",),
        ),
        Check(
            key="whitespace",
            name="Working tree diff",
            command=("git", "diff", "--check"),
            detail=(
                "Reject whitespace errors and conflict markers in unstaged changes."
            ),
            require_tools=("git",),
        ),
        Check(
            key="whitespace",
            name="Staged diff",
            command=("git", "diff", "--cached", "--check"),
            detail=(
                "Reject whitespace errors and conflict markers in staged changes "
                "that are about to be committed."
            ),
            require_tools=("git",),
        ),
    ]

    if skip_build:
        checks = [check for check in checks if check.key != "release"]

    if only is not None:
        selected = {name.strip().lower() for name in only.split(",") if name.strip()}

        known = {check.key for check in checks}
        unknown = selected - known
        if unknown:
            console.print(
                Panel(
                    "Unknown --only value(s): "
                    + ", ".join(sorted(unknown))
                    + f" (available: {', '.join(sorted(known))})",
                    title="[red]Cannot run prechecks[/red]",
                    border_style="red",
                )
            )
            raise SystemExit(2)

        checks = [check for check in checks if check.key in selected]

    return checks


def validate_environment(checks: Sequence[Check]) -> None:
    """Fail before doing work when an enabled check cannot run."""

    problems: list[str] = []

    required_tools = {tool for check in checks for tool in check.require_tools}
    for tool in sorted(required_tools):
        if shutil.which(tool) is None:
            problems.append(
                f"{tool} is not available on PATH (required by an enabled check)"
            )

    required_files = {file for check in checks for file in check.require_files}
    for file in sorted(required_files):
        if not (REPO_ROOT / file).is_file():
            problems.append(f"{file} was not found under {REPO_ROOT}")

    if not checks:
        problems.append("no prechecks are enabled")

    if problems:
        message = "\n".join(f"• {problem}" for problem in problems)
        console.print(
            Panel(
                message,
                title="[red]Cannot run prechecks[/red]",
                border_style="red",
            )
        )
        raise SystemExit(2)


def render_command(command: Sequence[str]) -> str:
    return shlex.join(str(part) for part in command)


def strip_terminal_codes(value: str) -> str:
    return ANSI_ESCAPE_PATTERN.sub("", value)


def print_process_line(line: str) -> None:
    """Render child output live while preserving any ANSI styling it contains."""

    value = line.rstrip("\r\n")
    if value:
        console.print(Text.from_ansi(value), soft_wrap=True)
    else:
        console.print()


def run_check(check: Check, *, position: int, total: int) -> CheckResult:
    console.print()
    console.rule(f"[bold cyan]{position}/{total} · {escape(check.name)}[/bold cyan]")
    console.print(check.detail)
    console.print(f"[dim]$ {escape(render_command(check.command))}[/dim]")

    started_at = time.monotonic()
    warning_lines: list[str] = []
    environment = os.environ.copy()
    process: subprocess.Popen[str] | None = None

    try:
        process = subprocess.Popen(
            list(check.command),
            cwd=REPO_ROOT,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,
        )
        assert process.stdout is not None

        for line in process.stdout:
            print_process_line(line)
            normalized = strip_terminal_codes(line).strip()
            if (
                check.reject_warnings
                and normalized
                and COMPILER_WARNING_PATTERN.search(normalized)
            ):
                warning_lines.append(normalized)

        returncode = process.wait()

    except KeyboardInterrupt:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()

        console.print("\n[yellow]Prechecks interrupted.[/yellow]")
        raise SystemExit(130)

    elapsed = time.monotonic() - started_at

    return CheckResult(
        check=check,
        returncode=returncode,
        elapsed_seconds=elapsed,
        warning_lines=tuple(dict.fromkeys(warning_lines)),
    )


def print_failure(result: CheckResult, *, remaining: Sequence[Check]) -> None:
    reasons: list[str] = []

    if result.returncode != 0:
        reasons.append(f"Command exited with status {result.returncode}.")

    if result.warning_lines:
        reasons.append(
            f"The build emitted {len(result.warning_lines)} unique warning "
            f"marker{'s' if len(result.warning_lines) != 1 else ''}:"
        )
        reasons.extend(f"  • {escape(line)}" for line in result.warning_lines)

    if remaining:
        reasons.append(
            "Fail-fast mode did not run: "
            + ", ".join(check.name for check in remaining)
        )

    console.print()
    console.print(
        Panel(
            "\n".join(reasons),
            title=f"[bold red]Failed · {escape(result.check.name)}[/bold red]",
            border_style="red",
        )
    )


def print_success(results: Sequence[CheckResult]) -> None:
    table = Table(box=box.SIMPLE, show_header=True, header_style="bold")
    table.add_column("Check")
    table.add_column("Result", justify="center")
    table.add_column("Time", justify="right")

    for result in results:
        table.add_row(
            result.check.name,
            "[green]PASS[/green]",
            f"{result.elapsed_seconds:.1f}s",
        )

    total_seconds = sum(result.elapsed_seconds for result in results)

    console.print()
    console.print(table)
    console.print(
        Panel(
            f"[bold green]All {len(results)} prechecks passed[/bold green] "
            f"in {total_seconds:.1f}s.",
            border_style="green",
        )
    )


def main() -> int:
    options = parse_args()

    checks = checks_for(
        skip_build=options.skip_build,
        only=options.only,
    )

    validate_environment(checks)

    console.print(
        Panel(
            "Quick gates run first; execution stops on the first failure. "
            "Cargo dependency resolution is locked, and both unstaged and staged "
            "Git diffs are checked before success is reported.",
            title="[bold]Hamstik Wheel prechecks[/bold]",
            border_style="cyan",
        )
    )

    results: list[CheckResult] = []

    for index, check in enumerate(checks):
        result = run_check(check, position=index + 1, total=len(checks))
        results.append(result)

        if not result.passed:
            print_failure(result, remaining=checks[index + 1 :])
            return result.returncode if result.returncode else 1

        console.print(
            f"[bold green]PASS[/bold green] {escape(check.name)} "
            f"[dim]({result.elapsed_seconds:.1f}s)[/dim]"
        )

    print_success(results)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())