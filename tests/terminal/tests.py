"""End-to-end checks of notmpc in a terminal. Each test gets the shared
private MPD (`mpd`) and the notmpc binary (`bin`)."""

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
    mpd.mpc("-q", "pause")
    subprocess.run([bin, "add", str(mpd.lib / BETA)], env=mpd.env, capture_output=True, check=True)
    assert "[paused]" in mpd.status() and mpd.current() == "Aone", mpd.status()
    mpd.reset(ALPHA)
    subprocess.run([bin, "add", str(mpd.lib / BETA)], env=mpd.env, capture_output=True, check=True)
    assert wait_for(lambda: mpd.current() == "Bone"), "stopped: the first new song starts"


def test_follow_mode_moves_and_restores(mpd, bin):
    mpd.reset(ALPHA, play=1)
    t = Term(bin, mpd)
    t.send("jj")  # browse cursor: Aone -> Athree
    t.send("t", settle=1.0)  # Athree plays next
    assert mpd.mpc("queued", "-f", "%title%").strip() == "Athree"
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
    assert "Output 1 (null) is disabled" in mpd.mpc("outputs")
    assert t.find("[ ]  null"), "the panel shows what MPD did"
    t.quit()
    mpd.mpc("-q", "enable", "1")


def test_mouse_tabs_and_seek(mpd, bin):
    mpd.reset(ALPHA, play=1)
    t = Term(bin, mpd)
    x, y = t.find("Directories")
    t.click(x + 1, y)
    assert t.find("Music"), "Directories breadcrumbs"
    x, y = t.find("Queue")
    t.click(x + 1, y)
    t.click(t.cols // 2, 2, settle=1.0)  # the progress bar
    elapsed = [l for l in mpd.status().splitlines() if "/" in l and "#" in l][0].split()[2].split("/")[0]
    m, s = map(int, elapsed.split(":"))
    assert 50 <= m * 60 + s <= 70, f"seeked to {elapsed} of 2:00"
    t.quit()


def test_mode_toggles_in_the_footer(mpd, bin):
    mpd.reset(ALPHA, play=1)
    mpd.mpc("repeat", "off")
    t = Term(bin, mpd)
    x, y = t.find("Repeat")
    assert y == t.rows - 1, f"modes sit on the footer row, found on row {y}"
    assert "Repeat" not in t.text().splitlines()[1], "and no longer in the header"
    t.click(x + 1, y)
    assert wait_for(lambda: "repeat: on" in mpd.status()), "clicking Repeat turns it on"
    x, y = t.find("Random")
    t.click(x + 1, y)
    assert wait_for(lambda: "random: on" in mpd.status()), "clicking Random turns it on"
    mpd.mpc("repeat", "off")
    mpd.mpc("random", "off")  # state leaks between tests
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
    t = Term(bin, mpd, cols=100, config="visualizer = false\nmarquee_speed = 10\n")
    footer = lambda: t.lines()[-1]
    seen = set()
    for _ in range(6):
        assert footer().startswith(" next: ") and "…" not in footer(), footer()
        seen.add(footer().split("[")[0])
        t.pump(0.3)
    assert len(seen) > 2, f"it moves: {seen}"
    t.quit()
