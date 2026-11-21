"""A bounded test-only process tree; the grandchild owns a loopback listener."""

import os
from pathlib import Path
import socket
import sys
import time


def run_listener():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        listener.listen(4)
        listener.settimeout(0.2)
        print("tree-stdout-ready", flush=True)
        print("tree-stderr-ready", file=sys.stderr, flush=True)
        Path("ready.tmp").write_text(
            f"{listener.getsockname()[1]} {os.getpgrp()} {os.getppid()} {os.getpid()}\n",
            encoding="ascii",
        )
        Path("ready.tmp").rename("ready")
        end = time.monotonic() + 12
        while time.monotonic() < end:
            try:
                conn, _ = listener.accept()
            except TimeoutError:
                continue
            with conn:
                conn.settimeout(0.2)
                try:
                    conn.sendall(b"tree-ready")
                except (ConnectionError, TimeoutError):
                    pass  # An independent port probe may close without reading.


def main():
    child = os.fork()
    if child == 0:
        grandchild = os.fork()
        if grandchild == 0:
            run_listener()
        else:
            os.waitpid(grandchild, 0)
    elif "early-exit" in sys.argv:
        end = time.monotonic() + 8
        while time.monotonic() < end:
            if Path("exit-leader").is_file():
                return
            time.sleep(0.02)
        raise SystemExit(1)
    else:
        os.waitpid(child, 0)


if __name__ == "__main__":
    main()
