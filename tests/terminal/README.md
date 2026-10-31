# Terminal tests

End-to-end checks: notmpc runs in a pseudo-terminal against a private MPD,
and each test checks what MPD did, what's on screen, or the process itself.

    cargo build --release
    python3 -m venv .venv && .venv/bin/pip install pyte
    .venv/bin/python tests/terminal/run.py            # all, ~70 s
    .venv/bin/python tests/terminal/run.py kitty cli  # names containing these

Needs `mpd`, `mpc` and `ffmpeg`. `NOTMPC_BIN` tests another binary.

The runner generates a small library with ffmpeg (three tagged albums, one
with a `cover.png`, and an untagged folder), starts MPD on a socket in a
temp directory (null output, plus a FIFO for the visualizer), and points
`MPD_HOST`, `XDG_RUNTIME_DIR`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and
`HOME` there, so neither notmpc nor `mpc` can reach your own MPD, config
or state. The directory is removed afterwards.

`Term` answers the queries notcurses sends at startup, as xterm (the
screen is rendered with pyte) or, with `kitty=True`, as kitty speaking its
keyboard protocol (keys go in as kitty encodes them, e.g. `\e[47;2u` for
Shift+/; there's no rendered screen then, so those tests check raw output
or MPD).
