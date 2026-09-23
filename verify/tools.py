#!/usr/bin/env python3
"""Fetch one pinned official proof-tool archive; never extract or execute it."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import tempfile
import time
import urllib.request


LOCK = Path(__file__).with_name("tools.lock.json")


def selected(tool: str, target: str) -> dict:
    lock = json.loads(LOCK.read_text())
    if lock["schema"] != "gobstopper.proof-tools.v1":
        raise ValueError("unrecognized tool lock schema")
    variants = lock["tools"][tool]
    pin = variants.get(target, variants.get("any"))
    if pin is None:
        raise ValueError("this proof-tool platform has no reviewed artifact pin")
    if (not pin["url"].startswith("https://github.com/")
            or "/releases/download/" not in pin["url"]
            or len(pin["sha256"]) != 64
            or any(char not in "0123456789abcdef" for char in pin["sha256"])
            or not 0 < pin["max_bytes"] <= 1024 * 1024 * 1024):
        raise ValueError("invalid reviewed tool pin")
    return pin


def copy_verified(stream, destination, pin, deadline):
    digest = hashlib.sha256()
    size = 0
    while True:
        if time.monotonic() >= deadline:
            raise TimeoutError("tool download deadline exceeded")
        # read1 performs at most one underlying read. HTTPResponse.read(n) can
        # internally accumulate a trickling response forever despite an idle
        # socket timeout, preventing our total deadline from being examined.
        block = stream.read1(min(1024 * 1024, pin["max_bytes"] - size + 1))
        if time.monotonic() >= deadline:
            raise TimeoutError("tool download deadline exceeded")
        if not block:
            break
        size += len(block)
        if size > pin["max_bytes"]:
            raise ValueError("tool archive exceeds reviewed size bound")
        digest.update(block)
        destination.write(block)
    if digest.hexdigest() != pin["sha256"]:
        raise ValueError("tool archive checksum mismatch")
    return size


def fetch(pin, output: Path):
    if output.exists() or output.is_symlink():
        raise FileExistsError("proof-tool output already exists")
    # New sibling only; link publication never clobbers an existing path.
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=output.parent, prefix=".proof-tool-", delete=False) as sink:
            temporary = Path(sink.name)
            deadline = time.monotonic() + 240
            with urllib.request.urlopen(pin["url"], timeout=20) as source:
                if not source.geturl().startswith("https://"):
                    raise ValueError("tool download redirected outside HTTPS")
                size = copy_verified(source, sink, pin, deadline)
            sink.flush()
            os.fsync(sink.fileno())
        os.link(temporary, output)
        return size
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tool", choices=("tlc", "java", "kani", "lean"))
    parser.add_argument("--output", type=Path, required=True, help="new file in an existing directory")
    args = parser.parse_args()
    target = f"{platform.system().lower()}-{platform.machine().lower()}"
    pin = selected(args.tool, target)
    size = fetch(pin, args.output.absolute())
    print(json.dumps({"schema": "gobstopper.proof-tool-fetch.v1", "tool": args.tool,
                      "target": target, "version": pin["version"], "url": pin["url"],
                      "sha256": pin["sha256"], "bytes": size,
                      "lock_sha256": hashlib.sha256(LOCK.read_bytes()).hexdigest()}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
