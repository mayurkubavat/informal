"""Subprocess CLI wrappers for opaque-box InFormal E2E tests."""

from __future__ import annotations

from dataclasses import dataclass
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
from typing import Any, Iterator, Mapping, Sequence


@dataclass(frozen=True)
class CliResult:
    """Result of invoking an InFormal CLI command or repository script."""

    returncode: int
    stdout: str
    stderr: str
    duration_s: float
    cmd: tuple[str, ...] = ()

    def __iter__(self) -> Iterator[Any]:
        """Support 4-tuple unpacking: rc, out, err, dur = run_...(...)."""
        return iter((self.returncode, self.stdout, self.stderr, self.duration_s))

    @property
    def combined(self) -> str:
        """Return stdout and stderr combined for substring diagnostics."""
        if self.stdout and self.stderr:
            return f"{self.stdout}\n{self.stderr}"
        return self.stdout or self.stderr

    def json(self) -> Any:
        """Parse stdout as JSON, raising AssertionError with output context on failure."""
        try:
            return json.loads(self.stdout)
        except json.JSONDecodeError as exc:
            raise AssertionError(
                f"Expected valid JSON on stdout from {list(self.cmd)!r} "
                f"(rc={self.returncode}): {exc}\n"
                f"--- stdout ---\n{self.stdout}\n--- stderr ---\n{self.stderr}"
            ) from exc


def _kill_process_group(proc: subprocess.Popen[str]) -> None:
    """Forcefully terminate a child process and its entire process group."""
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        return
    except OSError:
        try:
            proc.kill()
        except OSError:
            pass


def _effective_timeout(ctx: Any, timeout_s: float | None, default_s: float) -> float:
    base = float(default_s if timeout_s is None else timeout_s)
    scale = float(getattr(ctx, "timeout_scale", 1.0) or 1.0)
    return max(base * scale, 1.0)


def run_command(
    argv: Sequence[str | Path],
    *,
    cwd: Path,
    env: Mapping[str, str] | None = None,
    timeout_s: float = 60.0,
    check: bool = False,
) -> CliResult:
    """Execute a command with stdin=DEVNULL and process-group timeout cleanup."""
    cmd = tuple(str(arg) for arg in argv)
    start = time.monotonic()
    proc = subprocess.Popen(
        cmd,
        cwd=str(cwd),
        env=dict(env) if env is not None else None,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
        start_new_session=True,
    )
    try:
        stdout, stderr = proc.communicate(timeout=timeout_s)
    except subprocess.TimeoutExpired:
        _kill_process_group(proc)
        try:
            out, err = proc.communicate(timeout=2.0)
        except (subprocess.TimeoutExpired, OSError):
            out, err = "", ""
        raise TimeoutError(
            f"Command timed out after {timeout_s:.1f}s: {' '.join(cmd)}\n"
            f"--- stdout ---\n{out}\n--- stderr ---\n{err}"
        ) from None

    duration_s = time.monotonic() - start
    res = CliResult(
        returncode=proc.returncode,
        stdout=stdout,
        stderr=stderr,
        duration_s=duration_s,
        cmd=cmd,
    )
    if check and res.returncode != 0:
        raise AssertionError(
            f"Command failed with exit code {res.returncode}: {' '.join(cmd)}\n"
            f"--- stdout ---\n{res.stdout}\n--- stderr ---\n{res.stderr}"
        )
    return res


def run_shell_file(
    ctx: Any,
    script: str | Path,
    *,
    flavor: str | None = None,
    engines: str | Path | None = None,
    workdir: str | Path | None = None,
    commands: Sequence[str] = (),
    certificate: str | Path | None = None,
    extra_args: Sequence[str] = (),
    cwd: str | Path | None = None,
    timeout_s: float | None = None,
    check: bool = False,
    env: Mapping[str, str] | None = None,
) -> CliResult:
    """Run `informal shell <script>` with explicit --engines and --workdir."""
    shell_workdir = Path(workdir) if workdir is not None else (ctx.workdir / "shell_work")
    shell_workdir.mkdir(parents=True, exist_ok=True)
    engines_path = Path(engines) if engines is not None else ctx.engines_path

    argv: list[str] = [
        str(ctx.informal_bin),
        "shell",
        str(script),
        "--engines",
        str(engines_path),
        "--workdir",
        str(shell_workdir),
    ]
    if flavor is not None:
        argv.extend(["--flavor", flavor])
    for cmd in commands:
        argv.extend(["-c", cmd])
    if certificate is not None:
        argv.extend(["--certificate", str(certificate)])
    argv.extend(extra_args)

    return run_command(
        argv,
        cwd=Path(cwd) if cwd is not None else ctx.workdir,
        env=ctx.env if env is None else env,
        timeout_s=_effective_timeout(ctx, timeout_s, 60.0),
        check=check,
    )


def run_shell_commands(
    ctx: Any,
    commands: Sequence[str] | str,
    *,
    flavor: str | None = None,
    engines: str | Path | None = None,
    workdir: str | Path | None = None,
    extra_args: Sequence[str] = (),
    cwd: str | Path | None = None,
    timeout_s: float | None = None,
    check: bool = False,
    env: Mapping[str, str] | None = None,
) -> CliResult:
    """Run `informal shell -c ...` without ever entering interactive stdin REPL."""
    if isinstance(commands, str):
        cmd_list = [commands]
    else:
        cmd_list = list(commands)
    if not cmd_list:
        raise ValueError(
            "run_shell_commands requires at least one command; "
            "invoking `informal shell` without a script or -c enters the interactive REPL"
        )

    shell_workdir = Path(workdir) if workdir is not None else (ctx.workdir / "shell_work")
    shell_workdir.mkdir(parents=True, exist_ok=True)
    engines_path = Path(engines) if engines is not None else ctx.engines_path

    argv: list[str] = [
        str(ctx.informal_bin),
        "shell",
        "--engines",
        str(engines_path),
        "--workdir",
        str(shell_workdir),
    ]
    if flavor is not None:
        argv.extend(["--flavor", flavor])
    for cmd in cmd_list:
        argv.extend(["-c", cmd])
    argv.extend(extra_args)

    return run_command(
        argv,
        cwd=Path(cwd) if cwd is not None else ctx.workdir,
        env=ctx.env if env is None else env,
        timeout_s=_effective_timeout(ctx, timeout_s, 60.0),
        check=check,
    )


def run_shell_check(
    ctx: Any,
    script: str | Path,
    *,
    flavor: str | None = None,
    json_out: bool = True,
    json_output: bool | None = None,
    cwd: str | Path | None = None,
    timeout_s: float | None = None,
    check: bool = False,
    env: Mapping[str, str] | None = None,
) -> CliResult:
    """Run `informal shell <script> --check [--json]` dry-run script audit."""
    emit_json = json_output if json_output is not None else json_out
    argv: list[str] = [str(ctx.informal_bin), "shell", str(script), "--check"]
    if emit_json:
        argv.append("--json")
    if flavor is not None:
        argv.extend(["--flavor", flavor])

    return run_command(
        argv,
        cwd=Path(cwd) if cwd is not None else ctx.workdir,
        env=ctx.env if env is None else env,
        timeout_s=_effective_timeout(ctx, timeout_s, 30.0),
        check=check,
    )


def run_check_model(
    ctx: Any,
    model: str | Path,
    *,
    timeout_s: int = 60,
    wall_timeout_s: float | None = None,
    engines: str | Path | None = None,
    only: Sequence[str] | str = (),
    certificate: str | Path | None = None,
    certificate_dir: str | Path | None = None,
    cwd: str | Path | None = None,
    check: bool = False,
    env: Mapping[str, str] | None = None,
) -> CliResult:
    """Run `informal check <model>` with explicit --engines and bounded wall timeout."""
    engines_path = Path(engines) if engines is not None else ctx.engines_path
    cert = certificate if certificate is not None else certificate_dir

    argv: list[str] = [
        str(ctx.informal_bin),
        "check",
        str(model),
        "--engines",
        str(engines_path),
        "--timeout",
        str(int(timeout_s)),
    ]
    if only:
        only_val = only if isinstance(only, str) else ",".join(only)
        argv.extend(["--only", only_val])
    if cert is not None:
        argv.extend(["--certificate", str(cert)])

    default_wall = max(float(timeout_s) + 15.0, 30.0)
    eff_wall = (
        float(wall_timeout_s)
        if wall_timeout_s is not None
        else _effective_timeout(ctx, default_wall, default_wall)
    )
    return run_command(
        argv,
        cwd=Path(cwd) if cwd is not None else ctx.workdir,
        env=ctx.env if env is None else env,
        timeout_s=eff_wall,
        check=check,
    )


def run_lower(
    ctx: Any,
    files: Sequence[str | Path] | str | Path,
    *,
    top: str | None = None,
    defines: Sequence[str] = (),
    skip_initial_checks: bool = False,
    isunknown_is_false: bool = False,
    cwd: str | Path | None = None,
    timeout_s: float | None = None,
    check: bool = False,
    env: Mapping[str, str] | None = None,
) -> CliResult:
    """Run `informal lower <files...>` emitting Btor2 on stdout."""
    file_list = [files] if isinstance(files, (str, Path)) else list(files)
    argv: list[str] = [str(ctx.informal_bin), "lower", *(str(f) for f in file_list)]
    if top is not None:
        argv.extend(["--top", top])
    for define in defines:
        argv.extend(["-D", define])
    if skip_initial_checks:
        argv.append("--skip-initial-checks")
    if isunknown_is_false:
        argv.append("--isunknown-is-false")

    return run_command(
        argv,
        cwd=Path(cwd) if cwd is not None else ctx.workdir,
        env=ctx.env if env is None else env,
        timeout_s=_effective_timeout(ctx, timeout_s, 60.0),
        check=check,
    )


def run_docgen_check(
    ctx: Any,
    *,
    out: str | Path | None = None,
    llms_out: str | Path | None = None,
    timeout_s: float | None = None,
    check: bool = False,
    env: Mapping[str, str] | None = None,
) -> CliResult:
    """Run `informal docgen --check` with cwd=repo_root."""
    argv: list[str] = [str(ctx.informal_bin), "docgen", "--check"]
    if out is not None:
        argv.extend(["--out", str(out)])
    if llms_out is not None:
        argv.extend(["--llms-out", str(llms_out)])

    return run_command(
        argv,
        cwd=ctx.repo_root,
        env=ctx.env if env is None else env,
        timeout_s=_effective_timeout(ctx, timeout_s, 30.0),
        check=check,
    )


def run_repo_script(
    ctx: Any,
    script_rel: str | Path,
    args: Sequence[str | Path] = (),
    *,
    timeout_s: float | None = None,
    extra_env: Mapping[str, str] | None = None,
    check: bool = False,
) -> CliResult:
    """Run a repository Python or shell script with cwd=repo_root."""
    script_path = Path(script_rel)
    if not script_path.is_absolute():
        script_path = ctx.repo_root / script_path

    if script_path.suffix == ".py":
        argv: list[str] = [sys.executable, "-I", str(script_path), *(str(a) for a in args)]
    else:
        argv = ["bash", str(script_path), *(str(a) for a in args)]

    merged_env = dict(ctx.env)
    if extra_env:
        merged_env.update(extra_env)

    return run_command(
        argv,
        cwd=ctx.repo_root,
        env=merged_env,
        timeout_s=_effective_timeout(ctx, timeout_s, 120.0),
        check=check,
    )
