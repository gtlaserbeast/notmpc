"""Run notmpc in a pseudo-terminal against a private MPD.

`Mpd` builds a small music library with ffmpeg and starts its own MPD on
a socket in a temp directory; `Term` runs notmpc in a pty, answers the
terminal queries notcurses sends at startup (as xterm, or as kitty with
its keyboard protocol), and keeps a rendered screen (pyte) for xterm.
Nothing here touches the user's MPD, config or state: every path notmpc
could look at points into the temp directory, and the tests speak to the
private MPD over its socket themselves (no mpc needed).
"""

import fcntl
import os
import pty
import re
import select
import shutil
import socket
import struct
import subprocess
import tempfile
import termios
import time
from pathlib import Path

import pyte

SECONDS = 120  # length of every generated track


def wait_for(pred, timeout=5.0, step=0.05):
    """Polls `pred` until it's truthy; returns its last value."""
    end = time.time() + timeout
    while True:
        v = pred()
        if v or time.time() > end:
            return v
        time.sleep(step)


class Mpd:
    """A throwaway MPD with a generated library."""

    LIBRARY = {
        # folder: (album, album artist, date, [(title, artist)])
        "Artist A/Alpha (2001)": ("Alpha", "Artist A", "2001", [("Aone", "Artist A"), ("Atwo", "Artist A"), ("Athree", "Artist A")]),
        "Artist B/Beta (2005)": ("Beta", "Artist B", "2005", [("Bone", "Artist B"), ("Btwo", "Artist B")]),
        "Various/Gamma": ("Gamma", "Various Artists", "2010", [("Gone", "Xavier"), ("Gtwo", "Yolanda")]),
    }
    UNTAGGED = "[CAT 001] Someone - Loose Album - 1999"

    def __init__(self):
        self.dir = Path(tempfile.mkdtemp(prefix="notmpc-test-"))
        self.lib = self.dir / "music"
        self.run = self.dir / "run"
        (self.run / "mpd").mkdir(parents=True)
        self.socket = self.run / "mpd" / "socket"
        self.fifo = self.run / "mpd" / "visualizer.fifo"
        self.env = {
            **{k: v for k, v in os.environ.items() if k not in ("MPD_PORT",)},
            "MPD_HOST": str(self.socket),
            "XDG_RUNTIME_DIR": str(self.run),
            "XDG_CONFIG_HOME": str(self.dir / "config"),
            "XDG_STATE_HOME": str(self.dir / "state"),
            "HOME": str(self.dir / "home"),
        }
        self._make_library()
        conf = self.dir / "mpd.conf"
        conf.write_text(
            f'music_directory "{self.lib}"\n'
            f'db_file "{self.dir / "db"}"\n'
            f'log_file "{self.dir / "mpd.log"}"\n'
            f'bind_to_address "{self.socket}"\n'
            'audio_output {\n  type "null"\n  name "null"\n}\n'
            f'audio_output {{\n  type "fifo"\n  name "viz"\n  path "{self.fifo}"\n  format "44100:16:2"\n}}\n'
        )
        self.proc = subprocess.Popen(["mpd", "--no-daemon", str(conf)], env=self.env,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if not wait_for(self.socket.exists, 10):
            raise RuntimeError(f"MPD didn't start; see {self.dir / 'mpd.log'}")
        self.cmd("update")
        wait_for(lambda: "updating_db" not in self.status(), 30)

    def _make_library(self):
        def track(path, seconds, tags):
            meta = [a for k, v in tags.items() for a in ("-metadata", f"{k}={v}")]
            path.parent.mkdir(parents=True, exist_ok=True)
            subprocess.run(["ffmpeg", "-v", "error", "-f", "lavfi", "-i", f"sine=frequency=440:duration={seconds}",
                            "-map_metadata", "-1", *meta, "-c:a", "flac", str(path)], check=True)
        for folder, (album, aa, date, songs) in self.LIBRARY.items():
            for n, (title, artist) in enumerate(songs, 1):
                track(self.lib / folder / f"{n:02} {title}.flac", SECONDS, {
                    "title": title, "artist": artist, "album": album, "album_artist": aa,
                    "date": date, "track": n})
        subprocess.run(["ffmpeg", "-v", "error", "-f", "lavfi", "-i", "color=c=orange:s=64x64",
                        "-frames:v", "1", str(self.lib / "Artist A/Alpha (2001)/cover.png")], check=True)
        track(self.lib / self.UNTAGGED / "01  Loose One.flac", SECONDS, {})

    def cmd(self, command):
        """Sends one command to the private MPD; its reply as (key, value)
        pairs. Raises on an error (ACK)."""
        with socket.socket(socket.AF_UNIX) as sock:
            sock.connect(str(self.socket))
            f = sock.makefile("rwb")
            f.readline()  # OK MPD <version>
            f.write(command.encode() + b"\n")
            f.flush()
            pairs = []
            while (line := f.readline().decode().rstrip("\n")) != "OK":
                if line.startswith("ACK") or not line:
                    raise RuntimeError(f"MPD: {command}: {line or 'closed'}")
                k, _, v = line.partition(": ")
                pairs.append((k, v))
            return pairs

    def status(self):
        return dict(self.cmd("status"))

    def _titles(self, command):
        return [v for k, v in self.cmd(command) if k == "Title"]

    def queue(self):
        return self._titles("playlistinfo")

    def current(self):
        return next(iter(self._titles("currentsong")), "")

    def queued(self):
        """The next song's title, if one is due."""
        pos = self.status().get("nextsong")
        return self._titles(f"playlistinfo {pos}")[0] if pos else ""

    def outputs(self):
        """Output name -> enabled."""
        out, name = {}, None
        for k, v in self.cmd("outputs"):
            if k == "outputname":
                name = v
            elif k == "outputenabled":
                out[name] = v == "1"
        return out

    def reset(self, *folders, play=None):
        """Empty, plain-mode queue holding `folders` (default: everything);
        `play` starts the queue's `play`th song (from 1)."""
        for c in ("stop", "clear", "random 0", "repeat 0", "single 0", "consume 0", "enableoutput 0",
                  "enableoutput 1"):
            self.cmd(c)
        for f in folders or [""]:
            self.cmd(f'add "{f}"')
        if play is not None:
            self.cmd(f"play {play - 1}")

    def stop(self):
        self.proc.terminate()
        self.proc.wait(10)
        shutil.rmtree(self.dir, ignore_errors=True)


class Term:
    """notmpc in a pty. `kitty`: claim to be kitty and speak its keyboard
    protocol (no rendered screen then; check raw output instead), and with
    `graphics`, its graphics protocol too."""

    def __init__(self, binary, mpd, rows=30, cols=100, kitty=False, config="visualizer = false\nmarquee_speed = 0\n",
                 args=(), graphics=False):
        self.mpd, self.rows, self.cols, self.kitty, self.graphics = mpd, rows, cols, kitty, graphics
        cfg = Path(mpd.env["XDG_CONFIG_HOME"]) / "notmpc"
        cfg.mkdir(parents=True, exist_ok=True)
        (cfg / "config.toml").write_text(config)
        shutil.rmtree(mpd.env["XDG_STATE_HOME"], ignore_errors=True)
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.environ.clear()
            os.environ.update(mpd.env)
            os.environ["TERM"] = "xterm-kitty" if kitty else "xterm-256color"
            os.execv(binary, [binary, *args])
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, cols * 10, rows * 20))
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.ByteStream(self.screen)
        self.out = bytearray()
        self.pump(2.5)

    def pump(self, seconds=0.5):
        end = time.time() + seconds
        while time.time() < end:
            r, _, _ = select.select([self.fd], [], [], 0.03)
            if not r:
                continue
            try:
                d = os.read(self.fd, 1 << 20)
            except OSError:
                return
            self.out.extend(d)
            replies = {b"\x1b[6n": b"\x1b[1;1R", b"\x1b[c": b"\x1b[?62;22c", b"\x1b[0c": b"\x1b[?62;22c"}
            if self.kitty:
                replies |= {b"\x1b[>0q": b"\x1bP>|kitty(0.39.1)\x1b\\", b"\x1b[?u": b"\x1b[?11u",
                            b"\x1b[16t": b"\x1b[6;20;10t"}
                if self.graphics:
                    replies |= {b"\x1b_Gi=1,a=q;": b"\x1b_Gi=1;OK\x1b\\",
                                b"\x1b[14t": f"\x1b[4;{self.rows * 20};{self.cols * 10}t".encode()}
            # answered in the order asked: notcurses stops listening at
            # the device attributes reply, which it asks for last
            for q, a in sorted(((q, a) for q, a in replies.items() if q in d), key=lambda qa: d.index(qa[0])):
                os.write(self.fd, a)
            if not self.kitty:
                self.stream.feed(d)

    def send(self, keys, settle=0.6):
        """Types `keys` (str or bytes), one key at a time."""
        data = keys.encode() if isinstance(keys, str) else keys
        if isinstance(keys, str):
            for ch in keys:
                os.write(self.fd, ch.encode())
                self.pump(0.08)
        else:
            os.write(self.fd, data)
        self.pump(settle)

    def click(self, x, y, button=0, settle=0.6):
        os.write(self.fd, f"\x1b[<{button};{x + 1};{y + 1}M\x1b[<{button};{x + 1};{y + 1}m".encode())
        self.pump(settle)

    def lines(self):
        return ["".join(c.data for c in (self.screen.buffer[y][x] for x in range(self.cols))).rstrip()
                for y in range(self.rows)]

    def text(self):
        return "\n".join(self.lines())

    def find(self, s):
        """(x, y) of `s` on screen, or None."""
        for y, line in enumerate(self.lines()):
            if s in line:
                return line.index(s), y
        return None

    def output_since(self, mark):
        return re.sub(rb"\x1b\[[0-9;?<>=]*[A-Za-z]", b"", bytes(self.out[mark:]))

    def state(self):
        """Process state letter: S running, T stopped, Z exited."""
        try:
            return open(f"/proc/{self.pid}/stat").read().rsplit(")", 1)[1].split()[0]
        except FileNotFoundError:
            return "gone"

    def fds(self):
        try:
            return [os.readlink(f"/proc/{self.pid}/fd/{f}") for f in os.listdir(f"/proc/{self.pid}/fd")]
        except FileNotFoundError:
            return []

    def wakeups(self):
        n = 0
        for t in os.listdir(f"/proc/{self.pid}/task"):
            for line in open(f"/proc/{self.pid}/task/{t}/status"):
                if "ctxt_switches" in line:
                    n += int(line.split()[1])
        return n

    def quit(self):
        """Presses q; returns the exit status (killing it if it hangs)."""
        try:
            os.write(self.fd, b"q")
        except OSError:
            pass
        self.pump(1.0)
        pid, status = os.waitpid(self.pid, os.WNOHANG)
        if not pid:
            os.kill(self.pid, 9)
            _, status = os.waitpid(self.pid, 0)
        os.close(self.fd)
        return os.waitstatus_to_exitcode(status)
