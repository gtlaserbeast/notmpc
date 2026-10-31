# kitty: pixel images vanish after a resize (refresh clears the screen, then only re-places sprixels)

**Versions:** notcurses 3.0.8 (Gentoo `dev-cpp/notcurses-3.0.8-r1`), kitty
0.49.2, Linux. The code paths involved are unchanged on master (3.0.17):
`kitty_move`, `kitty_draw`, `notcurses_refresh` and the
`sigcont_seen_for_render` block in `notcurses_render` are identical.

## What happens

After the terminal is resized, any image drawn with `NCBLIT_PIXEL` on kitty
disappears and stays gone until the plane is blitted again. A resize that
leaves the cell grid the same size does it too: tiling window managers send
those often (Hyprland does).

## Why

1. The SIGWINCH handler sets `sigcont_seen_for_render`. On the next
   `notcurses_render()`, after rasterizing, it calls `notcurses_refresh()`
   (render.c, the `if(sigcont_seen_for_render)` block).
2. `notcurses_refresh()` → `clear_and_home()` emits `ED 2` (`\e[2J`).
3. In kitty, `ED 2` calls `grman_clear(self->grman, false, ...)`, which
   calls `filter_refs(..., free_images=true, clear_filter_func, ...)`. That
   removes every on-screen placement and **frees each image left with no
   placement** (kitty/graphics.c, `filter_refs`, the `remove_image_itr`
   branch). The image data is gone, not just hidden.
4. notcurses still treats the sprixel as loaded. The next render only
   re-places it, with `\e_Ga=p,i=<id>,p=1,q=2`, and never re-transmits it.
   kitty has no image with that id. The `ENOENT` reply is suppressed by
   `q=2`, so nothing visibly fails.

An explicit `notcurses_refresh()` from the application does the same:
clear, then re-place without re-sending.

## Reproduce

`kitty_resize.c` (attached) blits one 200×100 opaque image onto a 8×30
plane and renders in a loop:

```sh
cc -o kitty_resize kitty_resize.c $(pkg-config --cflags --libs notcurses)
./kitty_resize      # in kitty; resize the window, or: kill -WINCH <pid>
```

The orange rectangle disappears on the first resize.

To see the byte stream, `drive.py` runs the program in a pty that answers
notcurses' startup queries as kitty (XTVERSION, graphics query, cell
size), sends SIGWINCH at 2 s (same size) and 4 s (smaller), and logs the
graphics commands:

```sh
DRIVE_TERM=xterm-kitty python3 drive.py ./kitty_resize
```

```
 2.01 SIGWINCH (same size)
 2.01 out [2J
 2.11 out _Ga=p,i=13843119        <- placement of an image kitty just freed
 4.02 SIGWINCH (40x120 -> 35x100)
 4.02 out [2J
 4.12 out _Ga=p,i=13843119
```

The image is transmitted (`a=t`) once, at startup, and never again.

## Possible fixes

- After `clear_and_home()` on a kitty terminal, mark every sprixel
  `SPRIXEL_INVALIDATED`, so the next rasterization re-sends it instead of
  re-placing it. (Sixel probably needs nothing, since its glyph is the
  data.)
- Or avoid `ED 2` when pixel graphics are kitty: overwrite the cells
  instead, or delete placements explicitly with `a=d,d=a` (lowercase:
  placements only, data kept) and re-place.

## Workaround (applications)

Set `NCOPTION_NO_WINCH_SIGHANDLER`, handle SIGWINCH yourself, and after
your own `notcurses_refresh()` destroy and re-blit every pixel plane before
the next render.
