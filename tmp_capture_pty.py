#!/usr/bin/env python3
"""Temporary debug helper: capture raw PTY output bytes from a live agentmux
daemon for one agent, so a display bug can be replayed through
`parse_raw_debug`. Delete after use.

Usage:
    python3 tmp_capture_pty.py <agent_id> [out-file] [socket-path]

Then reproduce the bug in the agent pane and press Ctrl-C to stop.
The captured raw bytes are written verbatim (suitable for
`cargo run -p agentmux-terminal --example parse_raw_debug -- <out-file> <rows> <cols>`).
"""
import glob
import json
import os
import signal
import socket
import sys


def find_socket():
    tmp = os.environ.get("TMPDIR", "/tmp")
    cand = os.path.join(tmp, f"agentmux-{os.environ.get('USER','')}/agentmux.sock")
    if os.path.exists(cand):
        return cand
    hits = glob.glob(f"/var/folders/*/*/T/agentmux-{os.environ.get('USER','')}/agentmux.sock")
    return hits[0] if hits else None


def main():
    if len(sys.argv) < 2:
        print("usage: python3 tmp_capture_pty.py <agent_id> [out-file] [socket-path]")
        sys.exit(2)
    agent_id = sys.argv[1]
    out_path = sys.argv[2] if len(sys.argv) > 2 else "agy_capture.bin"
    sock_path = sys.argv[3] if len(sys.argv) > 3 else find_socket()
    if not sock_path:
        print("could not locate agentmux.sock; pass it as the 3rd arg")
        sys.exit(2)

    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(sock_path)

    def send(obj):
        s.sendall((json.dumps(obj) + "\n").encode())

    send({"type": "hello", "payload": {"client_version": "debug", "protocol": 3}})
    send({"id": "attach1", "version": 3, "type": "client.attach",
          "payload": {"agent_id": agent_id}})

    out = open(out_path, "wb")
    total = 0

    def finish(*_a):
        out.flush()
        out.close()
        print(f"\ncaptured {total} bytes -> {out_path}")
        sys.exit(0)

    signal.signal(signal.SIGINT, finish)
    print(f"capturing PTY bytes for {agent_id} -> {out_path}")
    print("reproduce the bug in the agent pane now, then press Ctrl-C")

    buf = b""
    while True:
        data = s.recv(65536)
        if not data:
            finish()
        buf += data
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except Exception:
                continue
            payload = msg.get("payload", msg) if isinstance(msg, dict) else {}
            if not isinstance(payload, dict):
                continue
            if payload.get("agent_id") == agent_id and isinstance(payload.get("bytes"), list):
                chunk = bytes(payload["bytes"])
                out.write(chunk)
                out.flush()
                total += len(chunk)


if __name__ == "__main__":
    main()
