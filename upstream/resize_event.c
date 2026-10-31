// NCKEY_RESIZE is delivered late when SIGWINCH lands on a thread other than
// notcurses' input thread. argv[1] == "thread": start a thread blocked in
// read(2) before notcurses_init (it inherits an unblocked SIGWINCH).
#include <notcurses/notcurses.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <fcntl.h>

static int pipefd[2];
static void* blocker(void* arg){ (void)arg; char c; for(;;){ if(read(pipefd[0], &c, 1) <= 0){ /* EINTR or done */ } } return NULL; }

static double now(void){ struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec / 1e9; }

int main(int argc, char** argv){
  if(argc > 1 && !strcmp(argv[1], "thread")){
    pipe(pipefd);
    pthread_t t; pthread_create(&t, NULL, blocker, NULL);
  }
  struct notcurses_options opts = { .flags = NCOPTION_SUPPRESS_BANNERS };
  struct notcurses* nc = notcurses_init(&opts, NULL);
  FILE* log = fopen("events.log", "w");
  double start = now();
  for(;;){
    notcurses_render(nc);
    ncinput ni;
    // an absolute CLOCK_MONOTONIC deadline, 100 ms out
    struct timespec ts; clock_gettime(CLOCK_MONOTONIC, &ts);
    ts.tv_nsec += 100000000; if(ts.tv_nsec >= 1000000000){ ts.tv_sec++; ts.tv_nsec -= 1000000000; }
    uint32_t id = notcurses_get(nc, &ts, &ni);
    if(id == NCKEY_RESIZE){ fprintf(log, "%.2f NCKEY_RESIZE\n", now() - start); fflush(log); }
    else if(id == 'q'){ fprintf(log, "%.2f q\n", now() - start); break; }
  }
  notcurses_stop(nc);
  return 0;
}
