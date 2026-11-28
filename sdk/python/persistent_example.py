"""Persistent mode: a function that is slow in its first iterations and fast afterwards.

Run: perfratio --persistent -w 0 -r 40 python3 persistent_example.py
Without perfratio the function runs once.
"""
import time

import perfratio_region as pr

calls = 0


def work():
    global calls
    calls += 1
    # a cold start that settles after 5 calls, like a JIT-compiled function
    time.sleep(0.04 if calls <= 5 else 0.01)


if __name__ == "__main__":
    pr.serve(work)
