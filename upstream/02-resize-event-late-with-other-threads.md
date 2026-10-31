# NCKEY_RESIZE arrives late (at the next keypress) if any thread existed before notcurses_init

**Versions:** notcurses 3.0.8, Linux. `input_thread`, `process_ibuf` and
`sigwinch_handler` are identical on master (3.0.17).

## What happens

If the program starts a thread before `notcurses_init()`, `NCKEY_RESIZE`
is often not delivered when the terminal is resized. It shows up only when
the next real input arrives (a keypress or a mouse event), and several
resizes in between merge into one event. That's common in practice: many
Rust programs, and anything that sets up a network thread first, start
threads early.

## Why

- `setup_signals()` blocks SIGWINCH with `pthread_sigmask` in the calling
  thread, and threads created later inherit that. A thread created
  *before* `notcurses_init()` keeps SIGWINCH unblocked, so the kernel may
  deliver the signal to it. The man page allows this ("interruption of
  some thread to process the signal").
- `sigwinch_handler` only sets `resize_seen`. The event is queued by
  `process_ibuf()` on the input thread, and that runs only once
  `block_on_input()` returns. When the signal landed on another thread,
  the input thread's `ppoll()` is never interrupted, so it sleeps until
  the next byte arrives on stdin.

(Rendering still picks up the new size, through `sigcont_seen_for_render`,
so the screen adapts. Only the input event is late, so applications that
rely on `NCKEY_RESIZE` to relayout or re-blit don't.)

## Reproduce

`resize_event.c` (attached) logs each `NCKEY_RESIZE` with a timestamp. With
`thread` as its argument, it first starts one thread blocked in `read(2)`:

```sh
cc -o resize_event resize_event.c $(pkg-config --cflags --libs notcurses) -lpthread
python3 drive.py ./resize_event          # SIGWINCH at 2 s and 4 s, q at 7 s
python3 drive.py ./resize_event thread
cat events.log
```

```
== no extra thread          == one thread started before notcurses_init
2.01 NCKEY_RESIZE           7.01 NCKEY_RESIZE      <- at the 'q', merged
4.01 NCKEY_RESIZE           7.01 q
7.02 q
```

## Possible fixes

- Have `sigwinch_handler` wake the input thread: `write(2)` a byte to a
  self-pipe that `block_on_input()` polls (async-signal-safe).
  Or, when the handler isn't running on the input thread,
  `pthread_kill()` the input thread.
- At least document that every thread must block SIGWINCH (or be created
  after `notcurses_init()`) for `NCKEY_RESIZE` to arrive promptly.
