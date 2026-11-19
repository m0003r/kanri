#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later

"""Send a JSON request to the running Kanri desktop app."""

import argparse
import json
import os
from pathlib import Path
import socket
import sys

MAX_REQUEST = 1024 * 1024


def default_socket():
    if sys.platform == "darwin":
        base = Path.home() / "Library" / "Application Support"
    else:
        base = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share")
    return base / "tech.trobonox.kanri" / "ipc" / "kanri.sock"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("request", nargs="?", help="JSON file (default: stdin)")
    parser.add_argument("--socket", type=Path, default=default_socket())
    args = parser.parse_args()

    if not hasattr(socket, "AF_UNIX"):
        parser.error("This client requires a Unix socket")
    try:
        if args.request:
            with open(args.request, "rb") as stream:
                data = stream.read(MAX_REQUEST + 1)
        else:
            data = sys.stdin.buffer.read(MAX_REQUEST + 1)
        if len(data) > MAX_REQUEST:
            raise ValueError("Request exceeds 1 MiB")
        command = json.loads(data)
        if not isinstance(command, dict):
            raise ValueError("Expected a JSON object")
        frame = json.dumps(command, allow_nan=False, separators=(",", ":")).encode() + b"\n"
        if len(frame) > MAX_REQUEST:
            raise ValueError("Request exceeds the 1 MiB wire limit")
    except (OSError, ValueError) as error:
        parser.error(str(error))

    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(30)
            try:
                connection.connect(str(args.socket))
            except OSError as error:
                print(f"Could not connect to Kanri at {args.socket}: {error}", file=sys.stderr)
                return 2
            connection.sendall(frame)
            with connection.makefile("rb") as stream:
                reply = json.loads(stream.readline())
            if not isinstance(reply, dict) or not isinstance(reply.get("ok"), bool):
                raise ValueError("Invalid reply")
    except (OSError, ValueError) as error:
        print(f"Kanri did not confirm the request. Check the board before retrying: {error}", file=sys.stderr)
        return 2

    print(json.dumps(reply, ensure_ascii=False))
    return 0 if reply["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
