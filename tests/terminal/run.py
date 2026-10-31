#!/usr/bin/env python3
"""Runs the terminal tests: python3 tests/terminal/run.py [NAME...]

Needs mpd, mpc and ffmpeg, Python with pyte, and a release build
(cargo build --release; or set NOTMPC_BIN). NAME picks tests by substring.
"""

import os
import sys
import time
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
try:
    import pyte  # noqa: F401
except ImportError:
    sys.exit("needs pyte: python3 -m venv .venv && .venv/bin/pip install pyte, then run with .venv/bin/python")

import tests
from harness import Mpd

repo = Path(__file__).resolve().parents[2]
binary = os.environ.get("NOTMPC_BIN", str(repo / "target/release/notmpc"))
if not os.access(binary, os.X_OK):
    sys.exit(f"no notmpc at {binary}: cargo build --release, or set NOTMPC_BIN")

names = [n for n in dir(tests) if n.startswith("test_")]
names.sort(key=lambda n: getattr(tests, n).__code__.co_firstlineno)
if sys.argv[1:]:
    names = [n for n in names if any(a in n for a in sys.argv[1:])]

print("starting a private MPD with a generated library…", flush=True)
mpd = Mpd()
failed = []
try:
    for n in names:
        t0 = time.time()
        try:
            getattr(tests, n)(mpd, binary)
            print(f"PASS {n} ({time.time() - t0:.1f}s)", flush=True)
        except Exception:
            failed.append(n)
            print(f"FAIL {n}\n" + "".join("    " + l for l in traceback.format_exc().splitlines(True)), flush=True)
finally:
    mpd.stop()
print(f"\n{len(names) - len(failed)} passed, {len(failed)} failed")
sys.exit(1 if failed else 0)
