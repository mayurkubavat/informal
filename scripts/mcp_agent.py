#!/usr/bin/env python3
"""A small agent that lets a chat model drive `informal mcp`.

It starts the server, lists its tools, and loops: the model answers with
one JSON object, either a tool call or its final answer; a tool's reply
goes back to the model as the next message. It needs only an
OpenAI-compatible chat endpoint (Ollama, llama-server, vLLM, a hosted
one), and no tool-calling support from the model: calls are plain JSON.

    scripts/mcp_agent.py "Open tests/fpv/fifo/fifo.jg.tcl. Which assertion fails, and why?"
    scripts/mcp_agent.py --mode auto --log run.jsonl --model gemma3:12b "..."

The key of a hosted endpoint is read from the environment variable named
by --api-key-env. The log gets one JSON line per step (the model's text,
the tool call, the reply), for measuring how a model uses the interface.
This is a reference client and a way to try a model, not an evaluation
harness: it has no sandbox and checks no answer.
"""
import argparse
import json
import os
import subprocess
import sys
import urllib.request

SYSTEM = """You are a formal verification engineer working with the InFormal tool \
through the tools listed below. Work in small steps and base every statement on a \
tool's reply, never on a guess.

Answer with exactly one JSON object and nothing else, in one of two forms:
  {"tool": "<name>", "arguments": {...}}    to call a tool
  {"answer": "<text>"}                      when you are done

%s

Tools:
%s"""


class Server:
    """`informal mcp` as a child process."""

    def __init__(self, binary, args):
        self.proc = subprocess.Popen(
            [binary, "mcp", *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True
        )
        self.next = 0

    def request(self, method, params=None):
        self.next += 1
        msg = {"jsonrpc": "2.0", "id": self.next, "method": method, "params": params or {}}
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()
        while True:
            line = self.proc.stdout.readline()
            if not line:
                sys.exit("the server ended")
            reply = json.loads(line)
            if reply.get("id") == self.next:
                if "error" in reply:
                    return {"isError": True, "content": [{"text": json.dumps(reply["error"])}]}
                return reply["result"]

    def call(self, name, arguments):
        """A tool's reply as (JSON text, failed)."""
        r = self.request("tools/call", {"name": name, "arguments": arguments})
        return r["content"][0]["text"], bool(r.get("isError"))


def describe(tool):
    """One tool for the prompt: name, arguments, what it does."""
    props = tool["inputSchema"].get("properties", {})
    required = tool["inputSchema"].get("required", [])
    args = []
    for name, schema in props.items():
        kind = schema.get("type", "")
        if "enum" in schema:
            kind = "|".join(schema["enum"])
        elif kind == "array":
            kind = "list"
        args.append(f"{name}{'' if name in required else '?'}: {kind}")
    return f"- {tool['name']}({', '.join(args)}): {tool['description']}"


def chat(url, key, model, messages, temperature):
    body = {
        "model": model,
        "messages": messages,
        "temperature": temperature,
        "response_format": {"type": "json_object"},
    }
    headers = {"Content-Type": "application/json"}
    if key:
        headers["Authorization"] = f"Bearer {key}"
    req = urllib.request.Request(f"{url}/chat/completions", json.dumps(body).encode(), headers)
    with urllib.request.urlopen(req, timeout=600) as r:
        reply = json.load(r)
    usage = reply.get("usage", {})
    return reply["choices"][0]["message"]["content"], usage


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("task")
    p.add_argument("--binary", default="target/release/informal")
    p.add_argument("--mode", default="propose", choices=["read-only", "propose", "auto"])
    p.add_argument("--root", default=".")
    p.add_argument("--workdir", default="informal_work/agent")
    p.add_argument("--base-url", default="http://127.0.0.1:11434/v1")
    p.add_argument("--model", default="gemma3:12b")
    p.add_argument("--api-key-env", default="")
    p.add_argument("--temperature", type=float, default=0.2)
    p.add_argument("--max-steps", type=int, default=16)
    p.add_argument("--max-reply", type=int, default=6000, help="characters of a tool reply shown to the model")
    p.add_argument("--log", help="write one JSON line per step here")
    a = p.parse_args()

    server = Server(a.binary, ["--mode", a.mode, "--root", a.root, "--workdir", a.workdir])
    init = server.request("initialize", {"protocolVersion": "2025-06-18"})
    tools = server.request("tools/list")["tools"]
    system = SYSTEM % (init.get("instructions", ""), "\n".join(describe(t) for t in tools))
    messages = [{"role": "system", "content": system}, {"role": "user", "content": a.task}]
    key = os.environ.get(a.api_key_env, "") if a.api_key_env else ""
    log = open(a.log, "w") if a.log else None
    tokens = 0
    answer = None
    for step in range(1, a.max_steps + 1):
        text, usage = chat(a.base_url, key, a.model, messages, a.temperature)
        tokens += usage.get("total_tokens", 0)
        messages.append({"role": "assistant", "content": text})
        record = {"step": step, "model": text}
        try:
            said = json.loads(text)
        except json.JSONDecodeError:
            said = None
        if isinstance(said, dict) and "answer" in said:
            answer = said["answer"]
            record["answer"] = answer
        elif isinstance(said, dict) and isinstance(said.get("tool"), str):
            reply, failed = server.call(said["tool"], said.get("arguments") or {})
            record.update(tool=said["tool"], arguments=said.get("arguments"), failed=failed, reply=reply)
            print(f"[{step}] {said['tool']} {json.dumps(said.get('arguments') or {})}"
                  f"{' -> error' if failed else ''}", file=sys.stderr)
            if len(reply) > a.max_reply:
                reply = reply[: a.max_reply] + " ... (cut: ask for less at a time)"
            messages.append({"role": "user", "content": f"Reply of {said['tool']}:\n{reply}"})
        else:
            record["failed"] = True
            messages.append({"role": "user", "content": 'Not understood. Answer with one JSON object: '
                             '{"tool": ..., "arguments": {...}} or {"answer": ...}.'})
        if log:
            log.write(json.dumps(record) + "\n")
            log.flush()
        if answer is not None:
            break
    server.proc.stdin.close()
    server.proc.wait()
    print(f"steps: {step}, tokens: {tokens}", file=sys.stderr)
    if answer is None:
        sys.exit(f"no answer in {a.max_steps} steps")
    print(answer)


if __name__ == "__main__":
    main()
