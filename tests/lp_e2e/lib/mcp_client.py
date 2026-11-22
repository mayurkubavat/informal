"""Opaque-box NDJSON JSON-RPC 2.0 client for `informal mcp`."""

from __future__ import annotations

import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import threading
import time
from typing import Any, Mapping


class McpProtocolError(AssertionError):
    """Raised on malformed NDJSON, unexpected EOF, or JSON-RPC error responses."""

    def __init__(
        self,
        message: str,
        *,
        code: int | None = None,
        raw: dict[str, Any] | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.raw = raw


class McpToolError(AssertionError):
    """Raised when `tools/call` returns `isError=True` and `expect_error=False`."""

    def __init__(
        self,
        tool_name: str,
        error_obj: dict[str, Any],
        raw_result: dict[str, Any],
    ) -> None:
        code = error_obj.get("code", "unknown")
        msg = error_obj.get("message", "tool returned isError=True")
        super().__init__(f"MCP tool '{tool_name}' failed ({code}): {msg}")
        self.tool_name = tool_name
        self.error_obj = error_obj
        self.code = code
        self.message = msg
        self.raw_result = raw_result


def parse_tool_error(raw_result: dict[str, Any]) -> dict[str, Any]:
    """Extract parsed `informal.error/1` JSON (or raw fallback) from an error result."""
    content = raw_result.get("content")
    if isinstance(content, list) and content:
        first = content[0]
        if isinstance(first, dict):
            text = first.get("text")
            if isinstance(text, str):
                try:
                    parsed = json.loads(text)
                    if isinstance(parsed, dict):
                        return parsed
                except json.JSONDecodeError:
                    pass
                return {"message": text, "raw_content": content}
    return {"message": "MCP tool error without text content", "raw_result": raw_result}


class McpClient:
    """Context manager speaking newline-delimited JSON-RPC 2.0 with `informal mcp`."""

    def __init__(
        self,
        informal_bin: str | Path,
        *,
        mode: str = "auto",
        root: str | Path,
        workdir: str | Path,
        engines: str | Path | None = None,
        max_time: int = 120,
        env: Mapping[str, str] | None = None,
        cwd: str | Path | None = None,
        default_timeout_s: float = 60.0,
        protocol_version: str = "2024-11-05",
        auto_initialize: bool = True,
    ) -> None:
        self.informal_bin = Path(informal_bin)
        self.mode = mode
        self.root = Path(root)
        self.workdir = Path(workdir)
        self.engines = Path(engines) if engines is not None else None
        self.max_time = int(max_time)
        self.env = dict(env) if env is not None else None
        self.cwd = Path(cwd) if cwd is not None else self.root
        self.default_timeout_s = float(default_timeout_s)
        self.protocol_version = protocol_version
        self.auto_initialize = auto_initialize

        self._proc: subprocess.Popen[str] | None = None
        self._rx_queue: queue.Queue[tuple[str, Any]] = queue.Queue()
        self._pending_responses: dict[Any, dict[str, Any]] = {}
        self.notifications: list[dict[str, Any]] = []
        self._stderr_lines: list[str] = []
        self._stderr_lock = threading.Lock()
        self._stdout_thread: threading.Thread | None = None
        self._stderr_thread: threading.Thread | None = None
        self._next_id = 0
        self._closed = False

    @property
    def stderr_text(self) -> str:
        with self._stderr_lock:
            return "".join(self._stderr_lines)

    def start(self) -> McpClient:
        """Spawn the `informal mcp` subprocess and start stdout/stderr reader threads."""
        if self._proc is not None:
            return self
        self.workdir.mkdir(parents=True, exist_ok=True)
        argv = [
            str(self.informal_bin),
            "mcp",
            "--mode",
            self.mode,
            "--root",
            str(self.root),
            "--workdir",
            str(self.workdir),
            "--max-time",
            str(self.max_time),
        ]
        if self.engines is not None:
            argv.extend(["--engines", str(self.engines)])

        self._closed = False
        self._proc = subprocess.Popen(
            argv,
            cwd=str(self.cwd),
            env=self.env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,
            start_new_session=True,
        )
        self._stdout_thread = threading.Thread(
            target=self._read_stdout,
            name="lp_e2e_mcp_stdout",
            daemon=True,
        )
        self._stderr_thread = threading.Thread(
            target=self._read_stderr,
            name="lp_e2e_mcp_stderr",
            daemon=True,
        )
        self._stdout_thread.start()
        self._stderr_thread.start()
        return self

    def _read_stdout(self) -> None:
        assert self._proc is not None and self._proc.stdout is not None
        try:
            for line in self._proc.stdout:
                stripped = line.strip()
                if not stripped:
                    continue
                try:
                    msg = json.loads(stripped)
                except json.JSONDecodeError as exc:
                    self._rx_queue.put(("err", f"Invalid JSON line {stripped!r}: {exc}"))
                    return
                if not isinstance(msg, dict):
                    self._rx_queue.put(("err", f"Non-object JSON-RPC line: {stripped!r}"))
                    return
                if msg.get("jsonrpc") != "2.0":
                    self._rx_queue.put(
                        ("err", f"Invalid jsonrpc version in message: {msg!r}")
                    )
                    return
                self._rx_queue.put(("msg", msg))
        except Exception as exc:  # pragma: no cover - defensive reader guard
            self._rx_queue.put(("err", str(exc)))
        finally:
            self._rx_queue.put(("eof", self._proc.poll()))

    def _read_stderr(self) -> None:
        assert self._proc is not None and self._proc.stderr is not None
        try:
            for line in self._proc.stderr:
                with self._stderr_lock:
                    self._stderr_lines.append(line)
        except Exception:
            pass

    def __enter__(self) -> McpClient:
        self.start()
        if self.auto_initialize:
            try:
                self.initialize()
            except Exception:
                self.close()
                raise
        return self

    def __exit__(self, exc_type: Any, exc: Any, tb: Any) -> None:
        self.close()

    def close(self, grace_s: float = 5.0) -> int | None:
        """Close stdin cleanly and kill the process group if it fails to exit."""
        if self._closed or self._proc is None:
            return self._proc.returncode if self._proc is not None else None
        self._closed = True
        proc = self._proc

        if proc.stdin is not None and not proc.stdin.closed:
            try:
                proc.stdin.close()
            except OSError:
                pass

        try:
            proc.wait(timeout=grace_s)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except OSError:
                try:
                    proc.kill()
                except OSError:
                    pass
            try:
                proc.wait(timeout=2.0)
            except subprocess.TimeoutExpired:
                pass

        for thread in (self._stdout_thread, self._stderr_thread):
            if thread is not None and thread.is_alive():
                thread.join(timeout=1.0)

        return proc.returncode

    def notify(self, method: str, params: dict[str, Any] | None = None) -> None:
        """Send a single-line NDJSON JSON-RPC 2.0 notification (no id)."""
        if self._proc is None or self._proc.stdin is None or self._proc.stdin.closed:
            raise McpProtocolError("Cannot send notification: MCP process is not running")
        payload: dict[str, Any] = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            payload["params"] = params
        line = json.dumps(payload, separators=(",", ":")) + "\n"
        try:
            self._proc.stdin.write(line)
            self._proc.stdin.flush()
        except OSError as exc:
            raise McpProtocolError(
                f"Failed writing notification {method}: {exc}; stderr:\n{self.stderr_text}"
            ) from exc

    def request(
        self,
        method: str,
        params: dict[str, Any] | None = None,
        *,
        timeout_s: float | None = None,
    ) -> dict[str, Any]:
        """Send a JSON-RPC 2.0 request and wait for the matching `id` response."""
        if self._proc is None or self._proc.stdin is None or self._proc.stdin.closed:
            raise McpProtocolError("Cannot send request: MCP process is not running")

        self._next_id += 1
        req_id = self._next_id
        payload: dict[str, Any] = {
            "jsonrpc": "2.0",
            "id": req_id,
            "method": method,
            "params": {} if params is None else params,
        }
        line = json.dumps(payload, separators=(",", ":")) + "\n"
        try:
            self._proc.stdin.write(line)
            self._proc.stdin.flush()
        except OSError as exc:
            raise McpProtocolError(
                f"Failed writing request {method} (id={req_id}): {exc}; "
                f"stderr:\n{self.stderr_text}"
            ) from exc

        eff_timeout = self.default_timeout_s if timeout_s is None else float(timeout_s)
        deadline = time.monotonic() + eff_timeout

        while True:
            if req_id in self._pending_responses:
                msg = self._pending_responses.pop(req_id)
                return self._extract_result(msg, method, req_id)

            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(
                    f"MCP request {method} (id={req_id}) timed out after "
                    f"{eff_timeout:.1f}s; stderr:\n{self.stderr_text}"
                )

            try:
                kind, data = self._rx_queue.get(timeout=remaining)
            except queue.Empty:
                raise TimeoutError(
                    f"MCP request {method} (id={req_id}) timed out after "
                    f"{eff_timeout:.1f}s; stderr:\n{self.stderr_text}"
                ) from None

            if kind == "eof":
                raise McpProtocolError(
                    f"MCP server closed stdout unexpectedly (rc={data}) waiting for "
                    f"{method} (id={req_id}); stderr:\n{self.stderr_text}"
                )
            if kind == "err":
                raise McpProtocolError(
                    f"Invalid NDJSON from MCP server while waiting for {method} "
                    f"(id={req_id}): {data}; stderr:\n{self.stderr_text}"
                )

            msg = data
            if "id" not in msg or msg["id"] is None:
                self.notifications.append(msg)
                continue
            if msg["id"] != req_id:
                self._pending_responses[msg["id"]] = msg
                continue
            return self._extract_result(msg, method, req_id)

    def _extract_result(
        self,
        msg: dict[str, Any],
        method: str,
        req_id: int,
    ) -> dict[str, Any]:
        if msg.get("jsonrpc") != "2.0":
            raise McpProtocolError(
                f"Expected jsonrpc=='2.0' in response to {method} (id={req_id}), got {msg!r}",
                raw=msg,
            )
        if msg.get("id") != req_id:
            raise McpProtocolError(
                f"Mismatched response id for {method}: expected {req_id}, got {msg.get('id')!r}",
                raw=msg,
            )
        if "error" in msg:
            err = msg["error"]
            code = err.get("code") if isinstance(err, dict) else None
            err_msg = err.get("message") if isinstance(err, dict) else str(err)
            raise McpProtocolError(
                f"MCP JSON-RPC error for {method} (id={req_id}, code={code}): "
                f"{err_msg}; stderr:\n{self.stderr_text}",
                code=code,
                raw=msg,
            )
        if "result" not in msg or not isinstance(msg["result"], dict):
            raise McpProtocolError(
                f"Expected dict 'result' in response to {method} (id={req_id}), got {msg!r}",
                raw=msg,
            )
        return msg["result"]

    def initialize(
        self,
        protocol_version: str | None = None,
        *,
        send_initialized: bool = True,
    ) -> dict[str, Any]:
        """Perform the MCP `initialize` handshake and optional `notifications/initialized`."""
        version = protocol_version or self.protocol_version
        result = self.request(
            "initialize",
            {
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": {"name": "lp_e2e", "version": "1.0"},
            },
        )
        for key in ("protocolVersion", "capabilities", "serverInfo"):
            if key not in result:
                raise McpProtocolError(
                    f"MCP initialize response missing '{key}': {result!r}",
                    raw=result,
                )
        if send_initialized:
            self.notify("notifications/initialized", {})
        return result

    def ping(self) -> dict[str, Any]:
        return self.request("ping", {})

    def tools_list(self) -> list[dict[str, Any]]:
        result = self.request("tools/list", {})
        tools = result.get("tools")
        if not isinstance(tools, list):
            raise McpProtocolError(f"tools/list missing 'tools' list: {result!r}", raw=result)
        return tools

    def tool_names(self) -> set[str]:
        return {str(t["name"]) for t in self.tools_list() if "name" in t}

    def prompts_list(self) -> list[dict[str, Any]]:
        result = self.request("prompts/list", {})
        prompts = result.get("prompts")
        if not isinstance(prompts, list):
            raise McpProtocolError(
                f"prompts/list missing 'prompts' list: {result!r}", raw=result
            )
        return prompts

    def prompts_get(
        self,
        name: str,
        arguments: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        params: dict[str, Any] = {"name": name}
        if arguments is not None:
            params["arguments"] = arguments
        return self.request("prompts/get", params)

    def resources_list(self) -> list[dict[str, Any]]:
        result = self.request("resources/list", {})
        resources = result.get("resources")
        if not isinstance(resources, list):
            raise McpProtocolError(
                f"resources/list missing 'resources' list: {result!r}", raw=result
            )
        return resources

    def resources_read(self, uri: str) -> dict[str, Any]:
        return self.request("resources/read", {"uri": uri})

    def call_tool_raw(
        self,
        name: str,
        arguments: dict[str, Any] | None = None,
        *,
        progress_token: int | str | None = None,
        timeout_s: float | None = None,
    ) -> dict[str, Any]:
        """Invoke `tools/call` and return the full MCP result envelope."""
        params: dict[str, Any] = {"name": name, "arguments": arguments or {}}
        if progress_token is not None:
            params["_meta"] = {"progressToken": progress_token}
        result = self.request("tools/call", params, timeout_s=timeout_s)
        if not isinstance(result.get("content"), list) or "isError" not in result:
            raise McpProtocolError(
                f"Malformed tools/call result for '{name}': {result!r}",
                raw=result,
            )
        return result

    def call_tool(
        self,
        name: str,
        arguments: dict[str, Any] | None = None,
        *,
        expect_error: bool = False,
        progress_token: int | str | None = None,
        timeout_s: float | None = None,
    ) -> dict[str, Any]:
        """Invoke `tools/call`.

        When `expect_error=False`, raises `McpToolError` if `isError=True` and
        returns `structuredContent` (or `result` if `structuredContent` is absent).
        When `expect_error=True`, asserts `isError=True` and returns parsed
        `informal.error/1` JSON (or raw content fallback).
        """
        raw = self.call_tool_raw(
            name,
            arguments,
            progress_token=progress_token,
            timeout_s=timeout_s,
        )
        is_error = bool(raw.get("isError"))
        if expect_error:
            if not is_error:
                raise AssertionError(
                    f"Expected MCP tool '{name}' to return isError=True, but it succeeded: {raw!r}"
                )
            return parse_tool_error(raw)

        if is_error:
            raise McpToolError(name, parse_tool_error(raw), raw)

        structured = raw.get("structuredContent")
        if isinstance(structured, dict):
            return structured
        return raw

    def progress_events(self, token: int | str | None = None) -> list[dict[str, Any]]:
        """Return buffered `notifications/progress` messages, optionally filtered by token."""
        events: list[dict[str, Any]] = []
        for msg in self.notifications:
            if msg.get("method") != "notifications/progress":
                continue
            params = msg.get("params")
            if token is not None:
                if not isinstance(params, dict) or params.get("progressToken") != token:
                    continue
            events.append(msg)
        return events

    def cancel(self, request_id: int) -> None:
        """Send `notifications/cancelled` for an in-flight request ID."""
        self.notify("notifications/cancelled", {"requestId": request_id})
