"""End-to-end checks of notmpc in a terminal. Each test gets the shared
private MPD (`mpd`) and the notmpc binary (`bin`)."""

import os
import subprocess
from pathlib import Path

from harness import Term, wait_for

ALPHA = "Artist A/Alpha (2001)"
BETA = "Artist B/Beta (2005)"
REPO = Path(__file__).resolve().parents[2]


def test_startup_shows_the_queue(mpd, bin):
    mpd.reset(play=1)
    t = Term(bin, mpd)
    text = t.text()
    assert "[Playing]" in text and "Aone" in text, text
    assert "Alpha (2001)" in text, "album header"
    assert "8 songs" in text, "footer"
    assert t.quit() == 0


def test_untagged_songs_get_names_from_the_path(mpd, bin):
    mpd.reset()
    t = Term(bin, mpd)
    text = t.text()
    t.quit()
    assert "Loose Album (1999)" in text and "Loose One" in text, text


def test_help_panel_opens_and_closes(mpd, bin):
    mpd.reset()
    t = Term(bin, mpd)
    t.send("?")
    assert t.find("Keys") and t.find("Global"), t.text()
    t.send("\x1b")
    assert not t.find("Global"), "Esc closes it"
    t.quit()


def test_kitty_shifted_symbols(mpd, bin):
    """kitty reports Shift+/ as / with Shift: it must still be ?."""
    mpd.reset(ALPHA, play=1)
    t = Term(bin, mpd, kitty=True)
    mark = len(t.out)
    t.send(b"\x1b[47;2u")  # Shift+/
    assert b"Keys" in t.output_since(mark), "? opens the keys panel"
    t.send(b"\x1b[27u")  # Esc
    t.send(b"\x1b[46;2u")  # Shift+. = > (next track), not volume up
    assert wait_for(lambda: mpd.current() == "Atwo"), mpd.current()
    t.quit()


def test_ctrl_z_and_ctrl_c_are_keys_not_signals(mpd, bin):
    mpd.reset()
    t = Term(bin, mpd, kitty=True)
    t.send(b"\x1b[122;5u", settle=1.0)  # Ctrl-Z in kitty's protocol
    assert t.state() in ("S", "R"), f"stopped: {t.state()}"
    t.send(b"\x1b[99;5u", settle=1.0)  # Ctrl-C
    assert t.state() in ("S", "R"), f"killed: {t.state()}"
    assert t.quit() == 0


def test_search_finds_and_plays(mpd, bin):
    mpd.reset(ALPHA)
    t = Term(bin, mpd)
    t.send("3")
    t.send('artist:"Artist B"', settle=1.0)
    assert t.find("2 songs in 1 album"), t.text()
    t.send("\x15")  # C-u clears the query
    t.send("Btwo", settle=1.0)
    assert t.find("1 song in 1 album"), t.text()
    t.send("\r")  # to the results
    t.send("\r", settle=1.0)  # play it
    assert wait_for(lambda: mpd.current() == "Btwo"), mpd.current()
    assert mpd.queue()[-1] == "Btwo", mpd.queue()
    t.quit()


def test_slash_jumps_to_search(mpd, bin):
    mpd.reset()
    t = Term(bin, mpd)
    t.send("/")
    assert t.find("Search: "), t.text()
    assert "isn't implemented" not in t.text()
    t.quit()


def test_directories_add_a_folder(mpd, bin):
    mpd.reset(ALPHA)
    t = Term(bin, mpd)
    t.send("2")
    assert t.find("Artist B"), t.text()
    # root, in natural order: [CAT 001]…, Artist A, Artist B, Various
    t.send("jj")
    t.send("a", settle=1.0)
    assert mpd.queue() == ["Aone", "Atwo", "Athree", "Bone", "Btwo"], mpd.queue()
    assert t.find("queued: 2 songs"), t.text()
    t.quit()


def test_adding_while_paused_keeps_the_paused_song(mpd, bin):
    mpd.reset(ALPHA, play=1)
    mpd.cmd("pause 1")
    subprocess.run([bin, "add", str(mpd.lib / BETA)], env=mpd.env, capture_output=True, check=True)
    assert mpd.status()["state"] == "pause" and mpd.current() == "Aone", mpd.status()
    mpd.reset(ALPHA)
    subprocess.run([bin, "add", str(mpd.lib / BETA)], env=mpd.env, capture_output=True, check=True)
    assert wait_for(lambda: mpd.current() == "Bone"), "stopped: the first new song starts"


def test_follow_mode_moves_and_restores(mpd, bin):
    mpd.reset(ALPHA, play=1)
    t = Term(bin, mpd)
    t.send("jj")  # browse cursor: Aone -> Athree
    t.send("t", settle=1.0)  # Athree plays next
    assert mpd.queued() == "Athree"
    assert mpd.queue() == ["Atwo", "Aone", "Athree"], "the playing song moved before the target"
    assert t.lines().index(next(l for l in t.lines() if "Aone" in l)) < t.lines().index(
        next(l for l in t.lines() if "Atwo" in l)), "the display keeps the real order"
    t.quit()
    assert mpd.queue() == ["Aone", "Atwo", "Athree"], "quitting puts it back"


def test_outputs_panel_toggles(mpd, bin):
    mpd.reset()
    t = Term(bin, mpd)
    t.send("oo")
    assert t.find("[x]  null"), t.text()
    t.send("\r", settle=1.0)
    assert mpd.outputs()["null"] is False, mpd.outputs()
    assert t.find("[ ]  null"), "the panel shows what MPD did"
    t.quit()
    mpd.cmd("enableoutput 0")


def test_mouse_tabs_and_seek(mpd, bin):
    mpd.reset(ALPHA, play=1)
    t = Term(bin, mpd)
    x, y = t.find("Directories")
    t.click(x + 1, y)
    assert t.find("Music"), "Directories breadcrumbs"
    x, y = t.find("Queue")
    t.click(x + 1, y)
    t.click(t.cols // 2, 2, settle=1.0)  # the progress bar
    elapsed = float(mpd.status()["elapsed"])
    assert 50 <= elapsed <= 70, f"seeked to {elapsed} s of 120"
    t.quit()


def test_mode_toggles_in_the_footer(mpd, bin):
    mpd.reset(ALPHA, play=1)
    t = Term(bin, mpd)
    x, y = t.find("Repeat")
    assert y == t.rows - 1, f"modes sit on the footer row, found on row {y}"
    assert "Repeat" not in t.text().splitlines()[1], "and no longer in the header"
    t.click(x + 1, y)
    assert wait_for(lambda: mpd.status()["repeat"] == "1"), "clicking Repeat turns it on"
    x, y = t.find("Random")
    t.click(x + 1, y)
    assert wait_for(lambda: mpd.status()["random"] == "1"), "clicking Random turns it on"
    mpd.cmd("repeat 0")
    mpd.cmd("random 0")  # state leaks between tests
    t.quit()


def test_visualizer_off_closes_the_fifo(mpd, bin):
    mpd.reset(play=1)
    t = Term(bin, mpd, config="visualizer = true\n")
    open_ = lambda: any("visualizer.fifo" in f for f in t.fds())
    assert wait_for(open_), "reading while on"
    t.send("V")
    assert wait_for(lambda: not open_(), 3), "closed while off"
    t.send("V")
    assert wait_for(open_, 3), "reading again"
    t.quit()


def test_cli(mpd, bin):
    run = lambda *a: subprocess.run([bin, *a], env=mpd.env, capture_output=True)
    assert run("--version").stdout.startswith(b"notmpc ")
    assert run("config").stdout == (REPO / "config.example.toml").read_bytes()
    assert run("bogus").returncode == 1
    mpd.reset(ALPHA)
    assert run("albumart", "-").returncode == 3, "nothing playing"
    mpd.reset(ALPHA, play=1)
    r = run("albumart", "-")
    assert r.returncode == 0 and r.stdout.startswith(b"\x89PNG"), "Alpha's cover.png"
    mpd.reset(BETA, play=1)
    assert run("albumart", "-").returncode == 2, "Beta has no cover"
    r = run("update", "--wait")
    assert r.returncode == 0 and b"done" in r.stderr, r.stderr


def test_cli_playback_controls(mpd, bin):
    run = lambda *a: subprocess.run([bin, *a], env=mpd.env, capture_output=True, check=True)
    mpd.reset(ALPHA)
    run("toggle")
    assert mpd.status()["state"] == "play", "toggle plays from stopped"
    run("next")
    assert mpd.current() == "Atwo"
    run("prev")
    assert mpd.current() == "Aone"
    run("toggle")
    assert mpd.status()["state"] == "pause"
    run("toggle")
    assert mpd.status()["state"] == "play" and mpd.current() == "Aone", "and resumes where it was"
    run("clear")
    assert mpd.queue() == [] and mpd.status()["state"] == "stop"


def test_marquee_scrolls_the_highlighted_row(mpd, bin):
    # In the flat table at this width the Artist column is 5 cells, so
    # "Artist A" is cut; only highlighted rows scroll instead.
    mpd.reset(ALPHA, play=1)
    cfg = "visualizer = false\ngroup_by_album = false\nart_width = 0\nmarquee_speed = {}\n"
    t = Term(bin, mpd, cols=50, config=cfg.format(1))  # slow: a cell a second
    row = lambda title: next(l for l in t.lines()[4:] if title in l)  # below the header
    t.send("j")
    t.send("k")  # the cursor moved: scrolling starts over (0.25 s rest, then cell 0 for 1 s)
    start = row("Aone")
    assert "Artis Aone" in start, start
    assert "Arti… Atwo" in row("Atwo"), "other rows keep the ellipsis"
    t.pump(1.0)  # a cell on
    assert "Artis" not in row("Aone") and "…" not in row("Aone"), row("Aone")
    t.send("j")  # the browse cursor joins: both start from the beginning
    assert "Artis Aone" in row("Aone") and "Artis Atwo" in row("Atwo"), t.text()
    t.quit()
    t = Term(bin, mpd, cols=50, config=cfg.format(0))
    assert "Arti… Aone" in row("Aone"), "0 turns it off"
    t.quit()


def test_a_broken_config_starts_with_the_defaults(mpd, bin):
    mpd.reset(ALPHA)
    t = Term(bin, mpd, config='marquee_speed = 0\n[theme]\nvisualizer_style = "blocks"\n')
    assert t.find("Aone"), "it runs: " + t.text()
    assert t.find("config.toml ignored") and t.find("move it above [theme]"), t.text()
    t.pump(5)
    assert t.find("config.toml ignored"), "it stays until a key"
    t.send("j")
    assert not t.find("config.toml ignored"), "a key dismisses it"
    assert t.quit() == 0


def test_marquee_scrolls_the_next_song_in_the_footer(mpd, bin):
    mpd.reset(ALPHA, play=1)
    t = Term(bin, mpd, cols=100, config="visualizer = false\nmarquee_speed = 10\nmarquee_count = \"infinite\"\n")
    footer = lambda: t.lines()[-1]
    seen = set()
    for _ in range(6):
        assert footer().startswith(" next: ") and "…" not in footer(), footer()
        seen.add(footer().split("[")[0])
        t.pump(0.3)
    assert len(seen) > 2, f"it moves: {seen}"
    t.quit()


def test_marquee_count_once_stops_at_the_ellipsis(mpd, bin):
    mpd.reset(ALPHA, play=1)
    t = Term(bin, mpd, cols=100, config="visualizer = false\nmarquee_speed = 10\n")  # once: the default
    footer = lambda: t.lines()[-1]
    # startup took 2.5 s: the one pass (0.25 s + ~1.7 s) is over
    assert "…" in footer(), footer()
    t.pump(1.0)
    assert "…" in footer(), "and it stays"
    mpd.cmd("next")  # a new next song: a new pass
    t.pump(0.6)
    assert "…" not in footer() and "Athree" in footer(), footer()
    t.quit()


def test_the_header_grows_with_the_visualizer(mpd, bin):
    mpd.reset(play=1)
    t = Term(bin, mpd, rows=24, config="visualizer = true\nmarquee_speed = 0\n")
    tabs = lambda: t.find("Directories")[1]
    title = lambda: next(y for y, l in enumerate(t.lines()) if "Loose One" in l)
    assert tabs() == 7 and title() == 2, t.text()  # 6 rows, title centred, bar, tabs
    t.send("V", settle=1.0)
    assert tabs() == 3 and title() == 0, t.text()
    t.quit()


def test_mouse_leaving_upwards_is_not_typed(mpd, bin):
    # kitty's pixel mouse reports go negative above the window; notcurses
    # can't parse those and passes them on as keys (`<` skips, `2` a tab).
    mpd.reset(ALPHA, play=2)
    t = Term(bin, mpd)
    for report in (b"\x1b[<288;512;-7M", b"\x1b[<288;-3;100M", b"\x1b[<32;1212;-40m"):
        os.write(t.fd, report)
        t.pump(0.5)
        assert mpd.current() == "Atwo" and not t.find("Music"), t.text()
    t.send("?")
    assert t.find("Global"), "keys still work"
    t.send("\x1b")
    assert not t.find("Global"), "and Esc alone is still Esc"
    t.send("[", settle=0.8)  # the same characters, one key at a time, still act
    assert t.find("the visualizer is"), t.text()
    t.quit()


def _state(mpd):
    p = Path(mpd.env["XDG_STATE_HOME"]) / "notmpc/state.toml"
    return p.read_text() if p.exists() else ""


def test_visualizer_tab_modes_and_idle_cost(mpd, bin):
    mpd.reset(ALPHA, play=1)
    t = Term(bin, mpd)  # header spectrum off
    reading = lambda: any("visualizer.fifo" in f for f in t.fds())
    t.pump(1.0)
    assert not reading()
    t.send("4", settle=1.0)
    assert t.find(" Spectrum ") and t.find(" Radial "), t.text()
    assert wait_for(reading, 3), "the tab reads the FIFO"
    t.send("l")
    assert 'scene = "Cover"' in _state(mpd), _state(mpd)
    t.send("h")
    t.send("h")
    assert 'scene = "Warp"' in _state(mpd), "wraps round: " + _state(mpd)
    assert t.find(" Warp ") and t.find("colours: theme"), "seven buttons and the switch fit: " + t.text()
    x, y = t.find(" Waterfall ")
    t.click(x + 2, y)
    assert 'scene = "Waterfall"' in _state(mpd), _state(mpd)
    t.send(" ")
    assert t.find("colours: cover") and "scene_cover_colors = true" in _state(mpd), t.text()
    t.send("1", settle=1.0)
    assert wait_for(lambda: not reading(), 3), "closed again on another tab"
    before = t.wakeups()
    t.pump(3.0)
    assert t.wakeups() - before < 40, f"{t.wakeups() - before} wakeups in 3 s on the queue tab"
    t.quit()


def test_visualizer_tab_frames_go_through_shared_memory_in_kitty(mpd, bin):
    import base64
    import re as re_
    import time

    frame = re_.compile(rb"\x1b_Ga=T,([^;]*);([A-Za-z0-9+/=]+)\x1b\\")
    mpd.reset(ALPHA, play=1)
    t = Term(bin, mpd, kitty=True, graphics=True)
    start = len(t.out)
    t.send(b"4", settle=0)
    # act as kitty: read each frame's pixels from shared memory, unlink it
    seen, sizes_ok, end = set(), True, time.time() + 2.0
    while time.time() < end:
        t.pump(0.03)
        for args, b64 in frame.findall(bytes(t.out[start:])):
            if b64 in seen:
                continue
            seen.add(b64)
            keys = dict(kv.split(b"=") for kv in args.split(b","))
            assert keys[b"t"] == b"s" and keys[b"z"] == b"-1", keys
            shm = Path("/dev/shm", base64.b64decode(b64).decode().lstrip("/"))
            if shm.exists():
                sizes_ok &= shm.stat().st_size == int(keys[b"s"]) * int(keys[b"v"]) * 4
                shm.unlink()
    assert len(seen) >= 2 and sizes_ok, (len(seen), sizes_ok)
    assert b"drawing in characters" not in bytes(t.out[start:]), "kept to kitty"
    mark = len(t.out)
    t.send(b"1", settle=0.8)
    assert b"a=d,d=I" in bytes(t.out[mark:]), "leaving the tab deletes the image"
    t.quit()
    left = list(Path("/dev/shm").glob(f"notmpc-{t.pid}-*"))
    assert not left, left


def test_a_stalled_mpd_does_not_leave_replies_one_behind(mpd, bin):
    # MPD waiting on a disk spinning up answers nobody, not even a new
    # connection's greeting. Once it wakes, the reply notmpc gave up on
    # must not be read as the next command's (the preview of the folder
    # above the cursor, from then on).
    import signal

    mpd.reset()
    t = Term(bin, mpd)
    t.send("2", settle=1.5)
    assert t.find("Loose Album (1999)"), t.text()
    os.kill(mpd.proc.pid, signal.SIGSTOP)
    try:
        t.send("j", settle=0.2)  # Artist A: its preview waits on the stuck MPD
        t.pump(23)  # the reply's 10 s timeout, then the reconnect's
    finally:
        os.kill(mpd.proc.pid, signal.SIGCONT)
    t.pump(1.5)
    t.send("j", settle=1.5)  # Artist B
    assert t.find("Beta (2005)") and not t.find("Alpha (2001)"), t.text()
    t.send("k", settle=1.5)  # Artist A
    assert t.find("Alpha (2001)") and not t.find("Beta (2005)"), t.text()
    t.quit()
