"""Pure-Python IEEE 1364 VCD parser and miter/LP trace verification helpers."""

from __future__ import annotations

from bisect import bisect_right
from dataclasses import dataclass, field
import fnmatch
from pathlib import Path
import re
from typing import Iterable

_GLUED_RANGE_RE = re.compile(r"^(.*?)(\[-?\d+:-?\d+\])$")
_SEPARATE_RANGE_RE = re.compile(r"^\[-?\d+(?::-?\d+)?\]$")
_LP_DOMAIN_RE = re.compile(r"(?:^|\.)lp\.pd\.([^.]+)\.")


@dataclass(frozen=True)
class VcdSignal:
    """Signal declaration inside a VCD trace."""

    name: str
    path: str
    width: int
    id: str
    var_type: str
    scope: tuple[str, ...]
    bit_range: str | None = None


def _fit_bits(raw: str, width: int) -> str:
    """IEEE 1364 left-extension or MSB truncation to exact `width`."""
    val = raw.lower()
    if len(val) >= width:
        return val[-width:]
    pad = val[0] if val and val[0] in ("x", "z") else "0"
    return (pad * (width - len(val))) + val


def _strip_leading_escape(ident: str) -> str:
    parts = [p[1:] if p.startswith("\\") else p for p in ident.split(".")]
    return ".".join(parts)


def _match_signal(sig: VcdSignal, pattern: str | re.Pattern[str]) -> bool:
    if isinstance(pattern, re.Pattern):
        return bool(pattern.search(sig.path))
    if (
        sig.path == pattern
        or _strip_leading_escape(sig.path) == _strip_leading_escape(pattern)
        or sig.name == pattern
        or _strip_leading_escape(sig.name) == _strip_leading_escape(pattern)
    ):
        return True
    if any(ch in pattern for ch in ("*", "?", "[")):
        if fnmatch.fnmatchcase(sig.path, pattern):
            return True
        if "." not in pattern and not pattern.startswith("*"):
            return fnmatch.fnmatchcase(sig.name, pattern)
        return False
    return False


@dataclass
class VcdTrace:
    """Parsed IEEE 1364 VCD trace."""

    timescale: str
    signals: list[VcdSignal]
    times: list[int]
    end_time: int
    comments: list[str]
    date: str = ""
    version: str = ""
    _by_path: dict[str, VcdSignal] = field(default_factory=dict, repr=False)
    _by_unescaped_path: dict[str, VcdSignal] = field(default_factory=dict, repr=False)
    _width_by_id: dict[str, int] = field(default_factory=dict, repr=False)
    _changes_by_id: dict[str, list[tuple[int, str]]] = field(default_factory=dict, repr=False)

    def get_signal(self, path: str) -> VcdSignal | None:
        """Look up a signal by exact path, unescaped path, or unique leaf name."""
        sig = self._by_path.get(path)
        if sig is not None:
            return sig
        sig = self._by_unescaped_path.get(_strip_leading_escape(path))
        if sig is not None:
            return sig
        matches = [
            s
            for s in self.signals
            if s.name == path
            or _strip_leading_escape(s.name) == _strip_leading_escape(path)
        ]
        if len(matches) == 1:
            return matches[0]
        return None

    def signal_paths(self) -> list[str]:
        """Return all declared hierarchical signal paths in declaration order."""
        return [s.path for s in self.signals]

    def find_signals(self, pattern: str | re.Pattern[str]) -> list[VcdSignal]:
        """Find all signals matching a glob, regex, exact path, or leaf name."""
        return [s for s in self.signals if _match_signal(s, pattern)]

    def _resolve_signal(self, signal: VcdSignal | str) -> VcdSignal:
        if isinstance(signal, VcdSignal):
            return signal
        resolved = self.get_signal(signal)
        if resolved is None:
            raise KeyError(f"Signal not found in VCD: {signal!r}")
        return resolved

    def changes(self, signal: VcdSignal | str) -> list[tuple[int, str]]:
        """Return chronological `(time, value)` changes for `signal`."""
        sig = self._resolve_signal(signal)
        return list(self._changes_by_id.get(sig.id, ()))

    def value_at(self, signal: VcdSignal | str, time: int) -> str | None:
        """Return signal value active at `time`, or `None` if before first change."""
        sig = self._resolve_signal(signal)
        series = self._changes_by_id.get(sig.id)
        if not series:
            return None
        idx = bisect_right(series, (time, "\uffff")) - 1
        if idx < 0:
            return None
        return series[idx][1]


def _consume_until_end(tokens: list[str], idx: int, directive: str) -> tuple[list[str], int]:
    collected: list[str] = []
    n = len(tokens)
    while idx < n:
        tok = tokens[idx]
        idx += 1
        if tok == "$end":
            return collected, idx
        collected.append(tok)
    raise ValueError(f"Unterminated {directive} directive in VCD (missing $end)")


def parse_vcd(path_or_text: str | Path) -> VcdTrace:
    """Parse an IEEE 1364 VCD file or raw VCD string using stdlib token streaming."""
    if isinstance(path_or_text, Path):
        text = path_or_text.read_text(encoding="utf-8", errors="replace")
    elif isinstance(path_or_text, str):
        stripped = path_or_text.lstrip()
        is_file = False
        if (
            "\n" not in path_or_text
            and len(path_or_text) < 4096
            and not stripped.startswith("$")
        ):
            try:
                is_file = Path(path_or_text).is_file()
            except OSError:
                is_file = False
        if is_file:
            text = Path(path_or_text).read_text(encoding="utf-8", errors="replace")
        else:
            text = path_or_text
    else:
        raise TypeError(
            f"parse_vcd expected str or Path, got {type(path_or_text).__name__}"
        )

    tokens = text.split()
    n = len(tokens)
    idx = 0

    timescale = ""
    date = ""
    version = ""
    comments: list[str] = []
    scope_stack: list[str] = []
    signals: list[VcdSignal] = []
    by_path: dict[str, VcdSignal] = {}
    by_unescaped_path: dict[str, VcdSignal] = {}
    width_by_id: dict[str, int] = {}
    seen_enddefinitions = False

    # Header phase
    while idx < n:
        tok = tokens[idx]
        idx += 1
        if tok == "$enddefinitions":
            _, idx = _consume_until_end(tokens, idx, "$enddefinitions")
            seen_enddefinitions = True
            break
        if tok == "$timescale":
            parts, idx = _consume_until_end(tokens, idx, "$timescale")
            timescale = "".join(parts)
        elif tok == "$date":
            parts, idx = _consume_until_end(tokens, idx, "$date")
            date = " ".join(parts)
        elif tok == "$version":
            parts, idx = _consume_until_end(tokens, idx, "$version")
            version = " ".join(parts)
        elif tok == "$comment":
            parts, idx = _consume_until_end(tokens, idx, "$comment")
            comments.append(" ".join(parts))
        elif tok == "$scope":
            parts, idx = _consume_until_end(tokens, idx, "$scope")
            if len(parts) < 2:
                raise ValueError(f"Malformed $scope directive: {parts!r}")
            scope_stack.append(parts[1])
        elif tok == "$upscope":
            _, idx = _consume_until_end(tokens, idx, "$upscope")
            if scope_stack:
                scope_stack.pop()
        elif tok == "$var":
            parts, idx = _consume_until_end(tokens, idx, "$var")
            if len(parts) < 4:
                raise ValueError(f"Malformed $var directive: {parts!r}")
            var_type, width_tok, id_code = parts[0], parts[1], parts[2]
            try:
                width = int(width_tok)
            except ValueError as exc:
                raise ValueError(f"Invalid $var width {width_tok!r}: {parts!r}") from exc
            if width < 1:
                raise ValueError(f"Invalid $var width {width}: {parts!r}")

            ref_tokens = parts[3:]
            bit_range: str | None = None
            if len(ref_tokens) >= 2 and _SEPARATE_RANGE_RE.match(ref_tokens[-1]):
                bit_range = ref_tokens[-1]
                raw_name = " ".join(ref_tokens[:-1])
            else:
                raw_name = " ".join(ref_tokens)
                if not raw_name.startswith("\\"):
                    m = _GLUED_RANGE_RE.match(raw_name)
                    if m and m.group(1):
                        raw_name = m.group(1)
                        bit_range = m.group(2)

            scope = tuple(scope_stack)
            path = ".".join((*scope, raw_name)) if scope else raw_name
            sig = VcdSignal(
                name=raw_name,
                path=path,
                width=width,
                id=id_code,
                var_type=var_type,
                scope=scope,
                bit_range=bit_range,
            )
            signals.append(sig)
            by_path.setdefault(path, sig)
            by_unescaped_path.setdefault(_strip_leading_escape(path), sig)
            width_by_id.setdefault(id_code, width)
        elif tok.startswith("$"):
            _, idx = _consume_until_end(tokens, idx, tok)
        else:
            raise ValueError(f"Unexpected token {tok!r} before $enddefinitions in VCD")

    if not seen_enddefinitions:
        raise ValueError("Input is not a valid VCD trace (missing $enddefinitions)")

    # Body (value-change) phase
    cur_time = 0
    end_time = 0
    changes_by_id: dict[str, list[tuple[int, str]]] = {}

    def record(id_code: str, val: str) -> None:
        series = changes_by_id.setdefault(id_code, [])
        if series and series[-1][0] == cur_time:
            if len(series) >= 2 and series[-2][1] == val:
                series.pop()
            else:
                series[-1] = (cur_time, val)
            return
        if not series or series[-1][1] != val:
            series.append((cur_time, val))

    while idx < n:
        tok = tokens[idx]
        idx += 1

        if tok in ("$dumpvars", "$dumpall", "$dumpon", "$dumpoff", "$end"):
            continue
        if tok in ("$comment", "$date", "$version"):
            _, idx = _consume_until_end(tokens, idx, tok)
            continue
        if tok.startswith("#"):
            try:
                cur_time = int(tok[1:])
            except ValueError as exc:
                raise ValueError(f"Invalid VCD timestamp token {tok!r}") from exc
            if cur_time > end_time:
                end_time = cur_time
            continue
        if tok.startswith(("b", "B")):
            if idx >= n:
                raise ValueError(f"Truncated vector change after {tok!r} in VCD")
            id_code = tokens[idx]
            idx += 1
            width = width_by_id.get(id_code, len(tok) - 1)
            record(id_code, _fit_bits(tok[1:], max(width, 1)))
            continue
        if tok.startswith(("r", "R", "s", "S")):
            if idx >= n:
                raise ValueError(f"Truncated real/string change after {tok!r} in VCD")
            id_code = tokens[idx]
            idx += 1
            prefix = tok[0].lower()
            record(id_code, prefix + tok[1:])
            continue
        if tok[0] in ("0", "1", "x", "X", "z", "Z") and len(tok) >= 2:
            id_code = tok[1:]
            val_char = tok[0].lower()
            width = width_by_id.get(id_code, 1)
            record(id_code, _fit_bits(val_char, width) if width > 1 else val_char)
            continue
        if tok.startswith("$"):
            _, idx = _consume_until_end(tokens, idx, tok)
            continue

    used_times = sorted(
        {t for series in changes_by_id.values() for t, _ in series}
    )

    return VcdTrace(
        timescale=timescale,
        signals=signals,
        times=used_times,
        end_time=end_time,
        comments=comments,
        date=date,
        version=version,
        _by_path=by_path,
        _by_unescaped_path=by_unescaped_path,
        _width_by_id=width_by_id,
        _changes_by_id=changes_by_id,
    )


def parse_vcd_file(path: str | Path) -> VcdTrace:
    """Parse an IEEE 1364 VCD file from disk, raising `FileNotFoundError` if missing."""
    if not isinstance(path, (str, Path)):
        raise TypeError(
            f"parse_vcd_file expected str or Path, got {type(path).__name__}"
        )
    p = Path(path)
    if not p.is_file():
        raise FileNotFoundError(f"VCD file not found: {p}")
    return parse_vcd(p)


def has_signal_matching(vcd: VcdTrace, pattern: str | re.Pattern[str]) -> bool:
    """Return True if at least one signal in `vcd` matches `pattern` (glob, regex, or name)."""
    return bool(vcd.find_signals(pattern))


def signal_changes_after(
    vcd: VcdTrace,
    signal_or_pattern: str | re.Pattern[str],
    after_time: int = 0,
) -> bool:
    """Return True if any matching signal transitions at timestamp `t > after_time`."""
    sigs = vcd.find_signals(signal_or_pattern)
    for sig in sigs:
        for t, _val in vcd.changes(sig):
            if t > after_time:
                return True
    return False


def _collect_auto_pairs(
    vcd: VcdTrace,
    prefix_a: str,
    prefix_b: str,
) -> list[tuple[str, VcdSignal, VcdSignal]]:
    pairs: list[tuple[str, VcdSignal, VcdSignal]] = []
    seen_pairs: set[tuple[str, str]] = set()
    for sig in vcd.signals:
        if sig.path.startswith(prefix_a):
            rel = sig.path[len(prefix_a) :]
            peer = vcd._by_path.get(prefix_b + rel)
            if peer is not None:
                key = (sig.path, peer.path)
                if key not in seen_pairs:
                    seen_pairs.add(key)
                    pairs.append((rel, sig, peer))

    if (prefix_a, prefix_b) == ("m0.", "m1."):
        for sig in vcd.signals:
            if sig.path.endswith("__m0"):
                base = sig.path[:-4]
                peer = vcd._by_path.get(base + "__m1")
                if peer is not None:
                    key = (sig.path, peer.path)
                    if key not in seen_pairs:
                        seen_pairs.add(key)
                        pairs.append((base, sig, peer))
    return pairs


def find_divergences(
    vcd: VcdTrace,
    sig_a: str | None = None,
    sig_b: str | None = None,
    *,
    prefix_a: str = "m0.",
    prefix_b: str = "m1.",
    after_time: int = 0,
) -> list[tuple[str, int, str | None, str | None]]:
    """Find `(signal_label, time, val_a, val_b)` divergences at `t >= after_time`."""
    pairs: list[tuple[str, VcdSignal, VcdSignal]] = []
    if sig_a is not None or sig_b is not None:
        if sig_a is None or sig_b is None:
            raise ValueError("Both sig_a and sig_b must be provided together")
        sa = vcd.get_signal(sig_a)
        if sa is None:
            raise KeyError(f"Signal not found in VCD: {sig_a!r}")
        sb = vcd.get_signal(sig_b)
        if sb is None:
            raise KeyError(f"Signal not found in VCD: {sig_b!r}")
        pairs.append((sa.path, sa, sb))
    else:
        pairs = _collect_auto_pairs(vcd, prefix_a, prefix_b)

    divergences: list[tuple[str, int, str | None, str | None]] = []
    for label, sa, sb in pairs:
        candidate_times: set[int] = {after_time}
        for t, _ in vcd.changes(sa):
            if t >= after_time:
                candidate_times.add(t)
        for t, _ in vcd.changes(sb):
            if t >= after_time:
                candidate_times.add(t)

        for t in sorted(candidate_times):
            va = vcd.value_at(sa, t)
            vb = vcd.value_at(sb, t)
            if va is None and vb is None:
                continue
            if va != vb:
                divergences.append((label, t, va, vb))
    return divergences


def signals_diverge(
    vcd: VcdTrace,
    sig_a: str | None = None,
    sig_b: str | None = None,
    *,
    prefix_a: str = "m0.",
    prefix_b: str = "m1.",
    after_time: int = 0,
) -> bool:
    """Return True if any paired miter/explicit signals diverge at `t >= after_time`."""
    return bool(
        find_divergences(
            vcd,
            sig_a,
            sig_b,
            prefix_a=prefix_a,
            prefix_b=prefix_b,
            after_time=after_time,
        )
    )


def lp_domain_names(vcd: VcdTrace) -> list[str]:
    """Return sorted unique domain names present as `lp.pd.<domain>.*` signals."""
    names: set[str] = set()
    for sig in vcd.signals:
        m = _LP_DOMAIN_RE.search(sig.path)
        if m:
            names.add(m.group(1))
    return sorted(names)


def has_lp_domain_signals(vcd: VcdTrace, domain: str | None = None) -> bool:
    """Return True if `vcd` contains `lp.pd.<domain>.*` signals (or any domain if None)."""
    names = lp_domain_names(vcd)
    if domain is None:
        return bool(names)
    return domain in names


def _iter_signal_paths(vcd: VcdTrace) -> Iterable[str]:
    return (s.path for s in vcd.signals)
