# Negative SGR mouse coordinates leak into the input as typed keys

**Affects:** notcurses 3.0.8; the patterns are unchanged in master (checked 2026-11-04).

**Terminal:** kitty 0.49.2, which supports SGR-pixel mouse reporting (mode 1016).

## What happens

kitty's pixel reports use the pointer's position relative to the window, so
they go negative when the pointer is above or left of it. When the pointer
leaves the window, kitty also sends a leave event at that position, for
example:

    ESC [ < 288 ; 512 ; -7 M

The input automaton's patterns accept digits only (`[<\N;\N;\NM` in `in.c`).
The `-` fails the match, and the whole sequence is delivered to the
application as separate key presses: Esc, `[`, `<`, `2`, `8`, `8`, `;`, `5`,
`1`, `2`, `;`, `-`, `7`, `M`. An application then runs those as commands.

Even when the report parses, `mouse_click()` leaves `id` at 0 for button codes
of 192 and up, which includes the leave code (288 = 256 + 32).

## Expected

Reports with negative coordinates are consumed. They should be dropped like
other reports outside the margins, or delivered with their position.

## Reproducing

Run a program with mouse events enabled in kitty, then move the pointer off
the top of the window. Or feed it the sequence above on its stdin.

## Workaround in notmpc

`Nc::get` holds an Esc for up to 10 ms and discards a sequence matching this
report's shape.
