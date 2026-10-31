// After SIGWINCH, notcurses_render() calls notcurses_refresh(), which clears
// the screen (ED 2) -- in kitty that frees the image -- and then only
// re-places existing sprixels (a=p) instead of re-transmitting them.
#include <notcurses/notcurses.h>
#include <stdint.h>
#include <stdlib.h>
#include <time.h>

int main(void){
  struct notcurses_options opts = { .flags = NCOPTION_SUPPRESS_BANNERS };
  struct notcurses* nc = notcurses_init(&opts, NULL);
  if(notcurses_check_pixel_support(nc) <= NCPIXEL_NONE){ notcurses_stop(nc); return 1; }
  enum { W = 200, H = 100 };
  uint32_t* px = malloc(W * H * 4);
  for(int i = 0 ; i < W * H ; ++i){ px[i] = 0xff0070dd; } // opaque orange (ABGR)
  struct ncvisual* v = ncvisual_from_rgba(px, H, W * 4, W);
  struct ncplane_options popts = { .y = 2, .x = 2, .rows = 8, .cols = 30 };
  struct ncplane* p = ncplane_create(notcurses_stdplane(nc), &popts);
  struct ncvisual_options vopts = { .n = p, .blitter = NCBLIT_PIXEL, .scaling = NCSCALE_STRETCH };
  ncvisual_blit(nc, v, &vopts);
  for(;;){
    notcurses_render(nc);
    ncinput ni;
    struct timespec ts; clock_gettime(CLOCK_MONOTONIC, &ts); // absolute deadline, 100 ms out
    ts.tv_nsec += 100000000; if(ts.tv_nsec >= 1000000000){ ts.tv_sec++; ts.tv_nsec -= 1000000000; }
    if(notcurses_get(nc, &ts, &ni) == 'q'){ break; }
  }
  notcurses_stop(nc);
  return 0;
}
