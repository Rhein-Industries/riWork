#!/usr/bin/python3 -I
"""The Security dashboard's worker: JSON lines on stdin and stdout (RiWork's worker protocol).

  {"id": 1, "method": "scan", "params": {"root": "/path", "skip": ["node_modules"]}}
  -> {"event": "progress", "data": {"files": 120}} ...  then  {"id": 1, "result": {"findings": [...], "files": 1234}}
"""
import json
import os
import re
import sys

PATTERNS = [
    ("AWS access key", re.compile(r"\bAKIA[0-9A-Z]{16}\b")),
    ("Private key", re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----")),
    ("GitHub token", re.compile(r"\bgh[pousr]_[A-Za-z0-9]{36,}\b")),
    ("Slack token", re.compile(r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b")),
    ("Anthropic key", re.compile(r"\bsk-ant-[A-Za-z0-9_-]{20,}\b")),
    ("Generic secret", re.compile(r"(?i)\b(?:api[_-]?key|secret|password|token)\b\s*[:=]\s*['\"][^'\"\s]{12,}['\"]")),
]
MAX_BYTES = 1_000_000


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def scan(root, skip):
    if not os.path.isabs(root) or not os.path.isdir(root):
        raise ValueError(f"{root} is not a folder")
    findings, files = [], 0
    for folder, dirs, names in os.walk(root):
        dirs[:] = [d for d in dirs if d not in skip and not d.startswith(".")]
        for name in names:
            path = os.path.join(folder, name)
            try:
                if os.path.getsize(path) > MAX_BYTES:
                    continue
                with open(path, "r", encoding="utf-8", errors="strict") as handle:
                    lines = handle.read().splitlines()
            except (OSError, UnicodeDecodeError):
                continue
            files += 1
            if files % 200 == 0:
                send({"event": "progress", "data": {"files": files}})
            for number, line in enumerate(lines, 1):
                for kind, pattern in PATTERNS:
                    if pattern.search(line):
                        findings.append({"file": os.path.relpath(path, root), "line": number, "kind": kind})
                        break
    return {"findings": findings[:500], "files": files}


for raw in sys.stdin:
    try:
        request = json.loads(raw)
    except ValueError:
        continue
    if not isinstance(request, dict) or "id" not in request:
        continue
    try:
        if request.get("method") != "scan":
            raise ValueError(f"unknown method {request.get('method')!r}")
        params = request.get("params") or {}
        skip = set(params.get("skip") or ["node_modules", "target", ".git"])
        send({"id": request["id"], "result": scan(params.get("root", ""), skip)})
    except Exception as error:  # noqa: BLE001 - reported to the add-in
        send({"id": request["id"], "error": {"message": str(error)}})
