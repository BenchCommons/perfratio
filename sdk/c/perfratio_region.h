/*
 * perfratio region client for C and C++ (protocol 1, SDK 1.0.0).
 * License: MIT OR Apache-2.0. Single header, no dependencies.
 *
 *   #include "perfratio_region.h"
 *   perfratio_region_start("sort");
 *   do_the_work();
 *   perfratio_region_stop("sort");
 *
 * In exactly ONE source file define PERFRATIO_REGION_IMPLEMENTATION before the
 * include; every other file includes the header as is. With -std=c11 (not
 * gnu11) also define _POSIX_C_SOURCE 200809L before any include.
 *
 * Run the program with `perfratio --region=sort ./program`. Without perfratio
 * (PERFRATIO_REGION_FILE unset or empty) every call returns immediately.
 * Events are buffered in memory and written when the program exits (atexit)
 * or on perfratio_region_flush(). A program that ends with abort(), a signal
 * or an uncaught C++ exception loses its buffered events: call
 * perfratio_region_flush() first when that matters.
 *
 * Not thread-safe: call from one thread or serialize calls yourself. Name a
 * region with 1-64 ASCII letters, digits or '_'; NULL means "default".
 * Wire format: docs/specs/region-protocol.md.
 */
#ifndef PERFRATIO_REGION_H
#define PERFRATIO_REGION_H

#define PERFRATIO_REGION_PROTOCOL 1
#define PERFRATIO_REGION_SDK_VERSION "1.0.0"
#define PERFRATIO_REGION_SDK_ID "c/" PERFRATIO_REGION_SDK_VERSION

#ifdef __cplusplus
extern "C" {
#endif

/* Non-zero when perfratio asked for region events. */
int perfratio_region_enabled(void);
/* Open the region `name`. */
void perfratio_region_start(const char *name);
/* Close the region `name`. */
void perfratio_region_stop(const char *name);
/* Write the buffered events now. Called automatically at exit. */
void perfratio_region_flush(void);

#ifdef __cplusplus
} /* extern "C" */

namespace perfratio {
/* RAII form: the region lasts as long as the object, also when an exception unwinds. */
class Region {
 public:
  explicit Region(const char *name = "default") : name_(name) { perfratio_region_start(name_); }
  ~Region() { perfratio_region_stop(name_); }

 private:
  Region(const Region &);
  Region &operator=(const Region &);
  const char *name_;
};
}  // namespace perfratio
#endif

#endif /* PERFRATIO_REGION_H */

#ifdef PERFRATIO_REGION_IMPLEMENTATION
#ifndef PERFRATIO_REGION_IMPLEMENTATION_DONE_
#define PERFRATIO_REGION_IMPLEMENTATION_DONE_

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#if defined(_WIN32)
#include <windows.h>
#else
#include <time.h>
#endif

typedef struct {
  char stop;      /* 0 = start, 1 = stop */
  char name[65];
  uint64_t t_ns;
} perfratio_region_event_;

static struct {
  int init;
  int hello;
  char *path;  /* NULL: disabled */
  perfratio_region_event_ *events;
  size_t len, cap, flushed;
  int warned;
} perfratio_region_state_;

/* Monotonic clock in nanoseconds: CLOCK_MONOTONIC, or QueryPerformanceCounter on Windows. */
static uint64_t perfratio_region_now_(void) {
#if defined(_WIN32)
  LARGE_INTEGER freq, count;
  QueryPerformanceFrequency(&freq);
  QueryPerformanceCounter(&count);
  return (uint64_t)(count.QuadPart / freq.QuadPart) * 1000000000ULL +
         (uint64_t)(count.QuadPart % freq.QuadPart) * 1000000000ULL / (uint64_t)freq.QuadPart;
#else
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
#endif
}

static int perfratio_region_valid_name_(const char *name) {
  size_t i, n = strlen(name);
  if (n == 0 || n > 64) return 0;
  for (i = 0; i < n; i++) {
    char c = name[i];
    if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '_'))
      return 0;
  }
  return 1;
}

void perfratio_region_flush(void) {
  FILE *file;
  size_t i;
  if (!perfratio_region_state_.path) return;
  file = fopen(perfratio_region_state_.path, "ab");
  if (!file) return; /* never break the program */
  if (!perfratio_region_state_.hello) {
    fprintf(file, "hello %d %s\n", PERFRATIO_REGION_PROTOCOL, PERFRATIO_REGION_SDK_ID);
    perfratio_region_state_.hello = 1;
  }
  for (i = perfratio_region_state_.flushed; i < perfratio_region_state_.len; i++) {
    const perfratio_region_event_ *e = &perfratio_region_state_.events[i];
    fprintf(file, "%s %s %llu\n", e->stop ? "stop" : "start", e->name, (unsigned long long)e->t_ns);
  }
  perfratio_region_state_.flushed = perfratio_region_state_.len;
  fclose(file);
}

static void perfratio_region_init_(void) {
  const char *path;
  if (perfratio_region_state_.init) return;
  perfratio_region_state_.init = 1;
  path = getenv("PERFRATIO_REGION_FILE");
  if (path && *path) {
    perfratio_region_state_.path = (char *)malloc(strlen(path) + 1);
    if (perfratio_region_state_.path) {
      strcpy(perfratio_region_state_.path, path);
      atexit(perfratio_region_flush);
    }
  }
}

int perfratio_region_enabled(void) {
  perfratio_region_init_();
  return perfratio_region_state_.path != NULL;
}

static void perfratio_region_record_(const char *name, int stop) {
  perfratio_region_event_ *e;
  uint64_t t = 0;
  if (!perfratio_region_enabled()) return;
  /* A stop reads the clock first and a start last, so the bookkeeping stays outside the region. */
  if (stop) t = perfratio_region_now_();
  if (!name) name = "default";
  if (!perfratio_region_valid_name_(name)) {
    if (!perfratio_region_state_.warned) {
      perfratio_region_state_.warned = 1;
      fprintf(stderr, "perfratio_region: ignoring invalid region name '%s' "
                      "(use 1-64 letters, digits or '_')\n", name);
    }
    return;
  }
  if (perfratio_region_state_.len == perfratio_region_state_.cap) {
    size_t cap = perfratio_region_state_.cap ? perfratio_region_state_.cap * 2 : 256;
    perfratio_region_event_ *grown = (perfratio_region_event_ *)realloc(
        perfratio_region_state_.events, cap * sizeof(perfratio_region_event_));
    if (!grown) return;
    perfratio_region_state_.events = grown;
    perfratio_region_state_.cap = cap;
  }
  e = &perfratio_region_state_.events[perfratio_region_state_.len++];
  e->stop = (char)stop;
  strcpy(e->name, name);
  e->t_ns = stop ? t : perfratio_region_now_();
}

void perfratio_region_start(const char *name) { perfratio_region_record_(name, 0); }
void perfratio_region_stop(const char *name) { perfratio_region_record_(name, 1); }

#endif /* PERFRATIO_REGION_IMPLEMENTATION_DONE_ */
#endif /* PERFRATIO_REGION_IMPLEMENTATION */
