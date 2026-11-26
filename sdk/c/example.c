/* Three phases of sleep; only the middle one is the region "middle".
 * Build: cc -std=c11 -D_POSIX_C_SOURCE=200809L example.c -o example
 * Run:   perfratio --region=middle ./example        (expected region time: 0.3 s)
 * Modes: example [exit]   "exit" leaves the region open and calls exit(3) (flush on exit). */
#define PERFRATIO_REGION_IMPLEMENTATION
#include "perfratio_region.h"

#include <stdlib.h>
#include <string.h>
#ifdef _WIN32
#include <windows.h>
static void sleep_ms(unsigned ms) { Sleep(ms); }
#else
#include <time.h>
static void sleep_ms(unsigned ms) {
  struct timespec ts;
  ts.tv_sec = ms / 1000;
  ts.tv_nsec = (long)(ms % 1000) * 1000000L;
  nanosleep(&ts, NULL);
}
#endif

int main(int argc, char **argv) {
  sleep_ms(100);
  perfratio_region_start("middle");
  sleep_ms(300);
  if (argc > 1 && strcmp(argv[1], "exit") == 0) exit(3);
  perfratio_region_stop("middle");
  sleep_ms(100);
  return 0;
}
