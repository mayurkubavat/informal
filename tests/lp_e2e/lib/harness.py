"""Core test harness, context, environment setup, and assertion helpers."""

from __future__ import annotations

from dataclasses import dataclass
import json
import os
from pathlib import Path
import shutil
from typing import Any, Callable, Mapping, Sequence

from . import cli_client
from .cli_client import CliResult
from .mcp_client import McpClient

REPO_ROOT: Path = Path(__file__).resolve().parents[3]
E2E_ROOT: Path = REPO_ROOT / "tests" / "lp_e2e"
FIXTURES_ROOT: Path = E2E_ROOT / "fixtures"

VALID_FEATURES: frozenset[str] = frozenset(f"F{i}" for i in range(1, 11))

_KEEP_INFORMAL_ENV_VARS: frozenset[str] = frozenset(
    {
        "INFORMAL_TOOLS_DIR",
        "INFORMAL_CADICAL_DIR",
        "INFORMAL_BITWUZLA_DIR",
        "INFORMAL_TCL_LIB",
        "INFORMAL_TCL_LIB_DIR",
        "INFORMAL_NO_SCCACHE",
    }
)


def normalize_features(feature: str | Sequence[str]) -> tuple[str, ...]:
    """Normalize `feature` (`str` or `Sequence[str]`) to a deduplicated tuple of `F1..F10`."""
    if isinstance(feature, str):
        raw_items = [feature]
    elif isinstance(feature, Sequence) and not isinstance(feature, (bytes, bytearray)):
        raw_items = list(feature)
    else:
        raise TypeError(
            f"TestCase.feature must be str or Sequence[str], got {type(feature).__name__}"
        )

    if not raw_items:
        raise ValueError("TestCase.feature cannot be empty")

    seen: set[str] = set()
    normalized: list[str] = []
    for item in raw_items:
        if not isinstance(item, str):
            raise TypeError(
                f"TestCase.feature items must be str, got {type(item).__name__}"
            )
        token = item.strip().upper()
        if token not in VALID_FEATURES:
            raise ValueError(
                f"Invalid feature {item!r}; expected one of {sorted(VALID_FEATURES)}"
            )
        if token not in seen:
            seen.add(token)
            normalized.append(token)
    return tuple(normalized)


@dataclass(frozen=True)
class TestCase:
    """Descriptor for an opaque-box E2E test case."""

    id: str
    tier: int
    feature: str | Sequence[str]
    title: str
    fn: Callable[[E2EContext], None]
    timeout_s: float = 60.0
    tags: tuple[str, ...] = ()

    @property
    def features(self) -> tuple[str, ...]:
        """Return `feature` normalized to a deduplicated tuple of feature IDs."""
        return normalize_features(self.feature)


def resolve_informal_bin(
    repo_root: Path = REPO_ROOT,
    explicit: Path | str | None = None,
) -> Path:
    """Resolve the compiled `informal` CLI binary path."""
    if explicit is not None:
        cand = Path(explicit).expanduser().resolve()
        if not cand.is_file() or not os.access(cand, os.X_OK):
            raise FileNotFoundError(
                f"Specified --bin is not an executable file: {cand}"
            )
        return cand

    env_bin = os.environ.get("INFORMAL_BIN", "").strip()
    if env_bin:
        cand = Path(env_bin).expanduser().resolve()
        if not cand.is_file() or not os.access(cand, os.X_OK):
            raise FileNotFoundError(
                f"INFORMAL_BIN is set to non-executable path: {cand}"
            )
        return cand

    for rel in ("target/debug/informal", "target/release/informal"):
        cand = (repo_root / rel).resolve()
        if cand.is_file() and os.access(cand, os.X_OK):
            return cand

    which_bin = shutil.which("informal")
    if which_bin:
        return Path(which_bin).resolve()

    raise FileNotFoundError(
        f"Could not locate `informal` binary under {repo_root / 'target'} or PATH. "
        "Build it first with `cargo build -p informal-cli` or pass `--bin <path>`."
    )


def resolve_engines_toml(
    repo_root: Path = REPO_ROOT,
    explicit: Path | str | None = None,
) -> Path:
    """Resolve `config/engines.toml` (or an explicit engines config path)."""
    cand = (
        Path(explicit).expanduser().resolve()
        if explicit is not None
        else (repo_root / "config" / "engines.toml").resolve()
    )
    if not cand.is_file():
        raise FileNotFoundError(f"Engines config file not found: {cand}")
    return cand


def build_tool_env(
    repo_root: Path = REPO_ROOT,
    base_env: Mapping[str, str] | None = None,
    *,
    keep_informal_vars: bool = False,
) -> dict[str, str]:
    """Build sanitized child env matching `scripts/check.sh` with local tools on PATH."""
    env = dict(os.environ if base_env is None else base_env)

    if not keep_informal_vars:
        for key in list(env.keys()):
            if key.startswith("INFORMAL_") and key not in _KEEP_INFORMAL_ENV_VARS:
                del env[key]

    tool_dirs: list[str] = []
    tools_dir_env = env.get("INFORMAL_TOOLS_DIR", "").strip()
    if tools_dir_env:
        tools_root = Path(tools_dir_env).expanduser()
        tool_dirs.extend(
            [
                str((tools_root / "rIC3" / "target" / "release").resolve()),
                str((tools_root / "abc").resolve()),
            ]
        )
    tool_dirs.extend(
        [
            str((repo_root / "tools" / "rIC3" / "target" / "release").resolve()),
            str((repo_root / "tools" / "abc").resolve()),
        ]
    )
    existing_dirs = [d for d in tool_dirs if Path(d).is_dir()]
    current_path = env.get("PATH", "")
    path_parts = [p for p in current_path.split(os.pathsep) if p]

    new_parts: list[str] = []
    for d in existing_dirs:
        if d not in new_parts:
            new_parts.append(d)
    for p in path_parts:
        if p not in new_parts:
            new_parts.append(p)
    env["PATH"] = os.pathsep.join(new_parts)
    return env


@dataclass
class E2EContext:
    """Isolated execution context passed to each E2E `TestCase` function."""

    repo_root: Path
    informal_bin: Path
    engines_path: Path
    workdir: Path
    env: dict[str, str]
    fixtures_dir: Path
    timeout_scale: float = 1.0

    @property
    def engines_toml(self) -> Path:
        """Alias for `engines_path`."""
        return self.engines_path

    def path(self, rel: str | Path) -> Path:
        """Resolve a relative path inside this test case's isolated workdir."""
        p = Path(rel)
        return p if p.is_absolute() else (self.workdir / p)

    def write_file(self, rel: str | Path, content: str) -> Path:
        """Write UTF-8 text into `workdir / rel`, creating parent directories."""
        dst = self.path(rel)
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_text(content, encoding="utf-8")
        return dst

    def write_json(self, rel: str | Path, data: Any) -> Path:
        """Serialize `data` as formatted JSON into `workdir / rel`."""
        return self.write_file(rel, json.dumps(data, indent=2, sort_keys=True) + "\n")

    def fixture(self, rel: str | Path) -> Path:
        """Resolve a fixture file or directory under `fixtures_dir` and verify it exists."""
        src = (self.fixtures_dir / rel).resolve()
        if not src.exists():
            raise FileNotFoundError(f"E2E fixture not found: {src}")
        return src

    def copy_fixture(
        self,
        rel: str | Path,
        dest_rel: str | Path | None = None,
    ) -> Path:
        """Copy a fixture file or directory tree into `workdir` so tracked files stay untouched."""
        src = self.fixture(rel)
        dst = self.path(dest_rel if dest_rel is not None else Path(rel).name)
        dst.parent.mkdir(parents=True, exist_ok=True)
        if src.is_dir():
            if dst.exists():
                shutil.rmtree(dst)
            shutil.copytree(src, dst)
        else:
            shutil.copy2(src, dst)
        return dst

    def cli_env(self, **extra_env: str) -> dict[str, str]:
        """Return a copy of `self.env` updated with extra environment variables."""
        merged = dict(self.env)
        merged.update(extra_env)
        return merged

    def run_shell_file(
        self,
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
        return cli_client.run_shell_file(
            self,
            script,
            flavor=flavor,
            engines=engines,
            workdir=workdir,
            commands=commands,
            certificate=certificate,
            extra_args=extra_args,
            cwd=cwd,
            timeout_s=timeout_s,
            check=check,
            env=env,
        )

    def run_shell_commands(
        self,
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
        return cli_client.run_shell_commands(
            self,
            commands,
            flavor=flavor,
            engines=engines,
            workdir=workdir,
            extra_args=extra_args,
            cwd=cwd,
            timeout_s=timeout_s,
            check=check,
            env=env,
        )

    def run_shell_check(
        self,
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
        return cli_client.run_shell_check(
            self,
            script,
            flavor=flavor,
            json_out=json_out,
            json_output=json_output,
            cwd=cwd,
            timeout_s=timeout_s,
            check=check,
            env=env,
        )

    def run_check_model(
        self,
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
        return cli_client.run_check_model(
            self,
            model,
            timeout_s=timeout_s,
            wall_timeout_s=wall_timeout_s,
            engines=engines,
            only=only,
            certificate=certificate,
            certificate_dir=certificate_dir,
            cwd=cwd,
            check=check,
            env=env,
        )

    def run_lower(
        self,
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
        return cli_client.run_lower(
            self,
            files,
            top=top,
            defines=defines,
            skip_initial_checks=skip_initial_checks,
            isunknown_is_false=isunknown_is_false,
            cwd=cwd,
            timeout_s=timeout_s,
            check=check,
            env=env,
        )

    def run_docgen_check(
        self,
        *,
        out: str | Path | None = None,
        llms_out: str | Path | None = None,
        timeout_s: float | None = None,
        check: bool = False,
        env: Mapping[str, str] | None = None,
    ) -> CliResult:
        return cli_client.run_docgen_check(
            self,
            out=out,
            llms_out=llms_out,
            timeout_s=timeout_s,
            check=check,
            env=env,
        )

    def run_repo_script(
        self,
        script_rel: str | Path,
        args: Sequence[str | Path] = (),
        *,
        timeout_s: float | None = None,
        extra_env: Mapping[str, str] | None = None,
        check: bool = False,
    ) -> CliResult:
        return cli_client.run_repo_script(
            self,
            script_rel,
            args,
            timeout_s=timeout_s,
            extra_env=extra_env,
            check=check,
        )

    def mcp(
        self,
        *,
        mode: str = "auto",
        root: str | Path | None = None,
        max_time: int = 120,
        engines: str | Path | None = None,
        workdir_name: str = "mcp_work",
        default_timeout_s: float = 60.0,
        auto_initialize: bool = True,
        env: Mapping[str, str] | None = None,
    ) -> McpClient:
        """Create an `McpClient` context manager rooted in `workdir` (by default)."""
        eff_root = Path(root) if root is not None else self.workdir
        eff_workdir = self.workdir / workdir_name
        eff_engines = Path(engines) if engines is not None else self.engines_path
        scaled_default = max(default_timeout_s * self.timeout_scale, 1.0)
        return McpClient(
            self.informal_bin,
            mode=mode,
            root=eff_root,
            workdir=eff_workdir,
            engines=eff_engines,
            max_time=max_time,
            env=self.env if env is None else env,
            cwd=eff_root,
            default_timeout_s=scaled_default,
            auto_initialize=auto_initialize,
        )


def assert_exit(res: CliResult | int, expected: int = 0, msg: str = "") -> None:
    """Assert CLI exit code matches `expected`, formatting stdout/stderr on mismatch."""
    if isinstance(res, CliResult):
        rc = res.returncode
        if rc != expected:
            prefix = f"{msg}: " if msg else ""
            raise AssertionError(
                f"{prefix}expected exit code {expected}, got {rc} for {list(res.cmd)!r}\n"
                f"--- stdout ---\n{res.stdout}\n--- stderr ---\n{res.stderr}"
            )
    else:
        if res != expected:
            prefix = f"{msg}: " if msg else ""
            raise AssertionError(f"{prefix}expected exit code {expected}, got {res}")


assert_rc = assert_exit


def assert_in(needle: str, haystack: str, msg: str = "") -> None:
    """Assert `needle` is a substring of `haystack`."""
    if needle not in haystack:
        prefix = f"{msg}: " if msg else ""
        raise AssertionError(
            f"{prefix}expected substring {needle!r} not found in:\n{haystack}"
        )


def assert_not_in(needle: str, haystack: str, msg: str = "") -> None:
    """Assert `needle` is NOT present in `haystack`."""
    if needle in haystack:
        prefix = f"{msg}: " if msg else ""
        raise AssertionError(
            f"{prefix}unexpected substring {needle!r} found in:\n{haystack}"
        )


def assert_contains(text: str, *substrings: str, msg: str = "") -> None:
    """Assert all `substrings` appear in `text`."""
    for sub in substrings:
        assert_in(sub, text, msg=msg)


def assert_not_contains(text: str, *substrings: str, msg: str = "") -> None:
    """Assert none of `substrings` appear in `text`."""
    for sub in substrings:
        assert_not_in(sub, text, msg=msg)


def assert_eq(actual: Any, expected: Any, msg: str = "") -> None:
    """Assert `actual == expected` with clear expected/actual values."""
    if actual != expected:
        prefix = f"{msg}: " if msg else ""
        raise AssertionError(f"{prefix}expected {expected!r}, got {actual!r}")


def assert_json_schema_field(obj: dict[str, Any], expected_schema: str) -> None:
    """Assert `obj['schema'] == expected_schema`."""
    actual = obj.get("schema")
    if actual != expected_schema:
        raise AssertionError(
            f"Expected schema {expected_schema!r}, got {actual!r} in {obj!r}"
        )


def assert_has_keys(obj: dict[str, Any], *keys: str) -> None:
    """Assert all `keys` exist in `obj`."""
    missing = [k for k in keys if k not in obj]
    if missing:
        raise AssertionError(f"Missing required keys {missing!r} in {obj!r}")
