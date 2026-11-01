#!/usr/bin/env python3
"""Sends one request to the gse service and prints every reply line.

usage: gse-request.py '{"request": "list"}'

Development aid: the app is the real client. Stops after Done or Error, or
on Ctrl+C for "watch".
"""

import json
import socket
import sys

SOCKET = "/run/gse/service.sock"


def main() -> int:
    request = sys.argv[1] if len(sys.argv) > 1 else '{"request": "list"}'
    json.loads(request)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.connect(SOCKET)
        s.sendall(request.encode() + b"\n")
        for line in s.makefile("rb"):
            reply = json.loads(line)
            print(json.dumps(reply), flush=True)
            if reply.get("reply") in ("done", "error"):
                return 0 if reply["reply"] == "done" else 1
    return 1


if __name__ == "__main__":
    sys.exit(main())
