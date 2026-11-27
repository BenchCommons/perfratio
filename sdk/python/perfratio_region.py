"""perfratio region client for Python (protocol 1, SDK 1.0.0).

License: MIT OR Apache-2.0. One file, standard library only.

    import perfratio_region as pr

    with pr.region("sort"):
        do_the_work()
    # or: pr.start("sort") ... pr.stop("sort")

Run the program with ``perfratio --region=sort python3 program.py``. Without
perfratio (``PERFRATIO_REGION_FILE`` unset or empty) every call returns at once.
Events are buffered in memory and written at interpreter exit (also after an
uncaught exception or ``sys.exit``) or on :func:`flush`. ``os._exit`` and fatal
signals skip the exit handlers: call :func:`flush` first when that matters.

Thread-safe. Names are 1-64 ASCII letters, digits or ``_``; the default name is
``"default"``. Wire format: docs/specs/region-protocol.md.
"""

import atexit
import contextlib
import os
import re
import sys
import threading
import time

PROTOCOL = 1
SDK_VERSION = "1.0.0"
SDK_ID = "python/" + SDK_VERSION
DEFAULT_REGION = "default"

_NAME = re.compile(r"[A-Za-z0-9_]{1,64}\Z")
# The monotonic clock of the protocol: CLOCK_MONOTONIC, mach_absolute_time or
# QueryPerformanceCounter, depending on the platform. On Windows time.monotonic_ns has a
# resolution of about 15.6 ms, so use perf_counter_ns there (QueryPerformanceCounter).
_clock = time.perf_counter_ns if sys.platform == "win32" else time.monotonic_ns


class Recorder:
    """Buffers events and appends them to ``path``. ``path=None`` disables it."""

    def __init__(self, path):
        self.path = path or None
        self._events = []
        self._flushed = 0
        self._hello = False
        self._warned = False
        # PERFRATIO_REGION_LIVE=1 (--region-scope): write every marker at once
        self._live = os.environ.get("PERFRATIO_REGION_LIVE") == "1"
        self._lock = threading.Lock()

    @property
    def enabled(self):
        return self.path is not None

    def start(self, name=DEFAULT_REGION):
        self._record("start", name)

    def stop(self, name=DEFAULT_REGION):
        self._record("stop", name)

    def _record(self, kind, name):
        if self.path is None:
            return
        # A stop reads the clock first and a start last, so the bookkeeping
        # stays outside the region.
        t = _clock() if kind == "stop" else 0
        if not isinstance(name, str) or not _NAME.match(name):
            if not self._warned:
                self._warned = True
                sys.stderr.write(
                    "perfratio_region: ignoring invalid region name %r "
                    "(use 1-64 letters, digits or '_')\n" % (name,)
                )
            return
        with self._lock:
            if kind == "start":
                t = _clock()
            self._events.append("%s %s %d\n" % (kind, name, t))
        if self._live:
            self.flush()

    def flush(self):
        if self.path is None:
            return
        with self._lock:
            pending = self._events[self._flushed :]
            first = not self._hello
            if not first and not pending:
                return
            text = ("hello %d %s\n" % (PROTOCOL, SDK_ID) if first else "") + "".join(pending)
            try:
                with open(self.path, "a", encoding="utf-8", newline="\n") as file:
                    file.write(text)
            except OSError:
                return  # never break the program
            self._hello = True
            self._flushed = len(self._events)

    @contextlib.contextmanager
    def region(self, name=DEFAULT_REGION):
        self.start(name)
        try:
            yield
        finally:
            self.stop(name)


_default = None
_default_lock = threading.Lock()


def _recorder():
    global _default
    if _default is None:
        with _default_lock:
            if _default is None:
                recorder = Recorder(os.environ.get("PERFRATIO_REGION_FILE"))
                if recorder.enabled:
                    atexit.register(recorder.flush)
                _default = recorder
    return _default


def enabled():
    """True when perfratio asked for region events."""
    return _recorder().enabled


def start(name=DEFAULT_REGION):
    """Open the region ``name``."""
    _recorder().start(name)


def stop(name=DEFAULT_REGION):
    """Close the region ``name``."""
    _recorder().stop(name)


def flush():
    """Write the buffered events now. Done automatically at exit."""
    _recorder().flush()


def region(name=DEFAULT_REGION):
    """Context manager: the region lasts for the ``with`` block, also on exceptions."""
    return _recorder().region(name)
