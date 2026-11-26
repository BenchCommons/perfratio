"""Three phases of sleep; only the middle one is the region "middle".

Run: perfratio --region=middle python3 example.py   (expected region time: 0.3 s)
Modes: example.py [raise]   "raise" raises inside the region (flush on exception).
"""
import sys
import time

import perfratio_region as pr

time.sleep(0.1)
with pr.region("middle"):
    time.sleep(0.3)
    if len(sys.argv) > 1 and sys.argv[1] == "raise":
        raise RuntimeError("boom")
time.sleep(0.1)
