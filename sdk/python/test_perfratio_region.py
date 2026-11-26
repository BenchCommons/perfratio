"""Unit tests of the Python region client: python3 -m unittest test_perfratio_region"""
import os
import subprocess
import sys
import tempfile
import time
import unittest

import perfratio_region as pr


def read(path):
    with open(path, encoding="utf-8") as file:
        return file.read()


class RecorderTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.path = os.path.join(self.dir.name, "events.log")

    def tearDown(self):
        self.dir.cleanup()

    def test_disabled_recorder_is_a_no_op(self):
        for path in (None, ""):
            recorder = pr.Recorder(path)
            self.assertFalse(recorder.enabled)
            recorder.start("a")
            recorder.stop("a")
            recorder.flush()
            with recorder.region("a"):
                pass
        self.assertFalse(os.path.exists(self.path))

    def test_buffers_until_flush_and_writes_hello_once(self):
        recorder = pr.Recorder(self.path)
        recorder.start("a")
        recorder.stop("a")
        self.assertFalse(os.path.exists(self.path))
        recorder.flush()
        with recorder.region("b"):
            pass
        recorder.flush()
        recorder.flush()
        lines = read(self.path).splitlines()
        self.assertEqual(lines[0], "hello 1 python/1.0.0")
        self.assertEqual([l.split()[0] for l in lines[1:]], ["start", "stop", "start", "stop"])
        self.assertEqual(sum(l.startswith("hello") for l in lines), 1)

    def test_hello_alone_marks_an_instrumented_program(self):
        pr.Recorder(self.path).flush()
        self.assertEqual(read(self.path), "hello 1 python/1.0.0\n")

    def test_clock_is_monotonic_and_measures_the_sleep(self):
        # the protocol needs the monotonic clock, never time.time()
        self.assertIs(pr._clock, time.monotonic_ns)
        recorder = pr.Recorder(self.path)
        with recorder.region("a"):
            time.sleep(0.02)
        recorder.flush()
        t = [int(line.split()[2]) for line in read(self.path).splitlines()[1:]]
        self.assertGreaterEqual(t[1] - t[0], 19_000_000)

    def test_region_stops_on_exception(self):
        recorder = pr.Recorder(self.path)
        with self.assertRaises(ValueError):
            with recorder.region("a"):
                raise ValueError("boom")
        recorder.flush()
        self.assertEqual([l.split()[0] for l in read(self.path).splitlines()[1:]], ["start", "stop"])

    def test_invalid_names_are_dropped_with_one_warning(self):
        recorder = pr.Recorder(self.path)
        stderr, sys.stderr = sys.stderr, open(os.devnull, "w")
        try:
            for name in ("", "a b", "a-b", "x" * 65, None, 3):
                recorder.start(name)
        finally:
            sys.stderr.close()
            sys.stderr = stderr
        recorder.flush()
        self.assertEqual(read(self.path), "hello 1 python/1.0.0\n")

    def test_unwritable_file_never_raises(self):
        recorder = pr.Recorder(os.path.join(self.dir.name, "missing", "events.log"))
        recorder.start("a")
        recorder.flush()


class ModuleTests(unittest.TestCase):
    def run_script(self, code, env_path):
        env = dict(os.environ)
        env.pop("PERFRATIO_REGION_FILE", None)
        if env_path is not None:
            env["PERFRATIO_REGION_FILE"] = env_path
        here = os.path.dirname(os.path.abspath(__file__))
        return subprocess.run(
            [sys.executable, "-c", "import sys; sys.path.insert(0, %r)\n%s" % (here, code)],
            env=env,
            capture_output=True,
            text=True,
        )

    def test_without_the_variable_nothing_happens(self):
        result = self.run_script(
            "import perfratio_region as pr\n"
            "pr.start('a'); pr.stop('a'); pr.flush()\n"
            "assert not pr.enabled()\n",
            None,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")

    def test_flush_at_exit_after_sys_exit_and_uncaught_exception(self):
        for ending in ("sys.exit(3)", "raise RuntimeError('boom')"):
            with tempfile.TemporaryDirectory() as directory:
                path = os.path.join(directory, "events.log")
                result = self.run_script(
                    "import perfratio_region as pr\n"
                    "with pr.region('a'):\n    pass\n"
                    "pr.start('open')\n" + ending + "\n",
                    path,
                )
                self.assertNotEqual(result.returncode, 0)
                lines = read(path).splitlines()
                self.assertEqual(lines[0], "hello 1 python/1.0.0")
                self.assertEqual(
                    [l.split()[:2] for l in lines[1:]],
                    [["start", "a"], ["stop", "a"], ["start", "open"]],
                )


if __name__ == "__main__":
    unittest.main()
