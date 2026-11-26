// Same three phases with the RAII form. Build: c++ -std=c++11 example.cpp -o example_cpp
// Modes: example_cpp [throw]   "throw" throws inside the region and catches in main.
#define PERFRATIO_REGION_IMPLEMENTATION
#include "perfratio_region.h"

#include <chrono>
#include <cstring>
#include <stdexcept>
#include <thread>

static void sleep_ms(int ms) { std::this_thread::sleep_for(std::chrono::milliseconds(ms)); }

int main(int argc, char **argv) {
  sleep_ms(100);
  try {
    perfratio::Region region("middle");
    sleep_ms(300);
    if (argc > 1 && std::strcmp(argv[1], "throw") == 0) throw std::runtime_error("boom");
  } catch (const std::exception &) {
    return 4;
  }
  sleep_ms(100);
  return 0;
}
