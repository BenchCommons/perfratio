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
        self.assertEqual(lines[0], "hello 1 python/1.1.0")
        self.assertEqual([l.split()[0] for l in lines[1:]], ["start", "stop", "start", "stop"])
        self.assertEqual(sum(l.startswith("hello") for l in lines), 1)

    def test_hello_alone_marks_an_instrumented_program(self):
        pr.Recorder(self.path).flush()
        self.assertEqual(read(self.path), "hello 1 python/1.1.0\n")

    def test_clock_is_monotonic_and_measures_the_sleep(self):
        # the protocol needs the monotonic clock, never time.time()
        self.assertIs(
            pr._clock,
            time.perf_counter_ns if sys.platform == "win32" else time.monotonic_ns,
        )
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
        self.assertEqual(read(self.path), "hello 1 python/1.1.0\n")

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
                self.assertEqual(lines[0], "hello 1 python/1.1.0")
                self.assertEqual(
                    [l.split()[:2] for l in lines[1:]],
                    [["start", "a"], ["stop", "a"], ["start", "open"]],
                )


class ServeTests(unittest.TestCase):
    def serve(self, requests, function):
        import io

        output = io.StringIO()
        pr.serve(function, io.StringIO(requests), output)
        return [l[len("@perfratio ") :] for l in output.getvalue().splitlines()]

    def test_every_run_is_answered_with_hello_and_an_iteration_region(self):
        calls = []
        lines = self.serve("run 0\nrun 1\nquit\nrun 2\n", lambda: calls.append(1))
        self.assertEqual(len(calls), 2)
        self.assertEqual(lines[0], "hello 1 python/1.1.0")
        kinds = [l.split()[:2] for l in lines[1:]]
        self.assertEqual(
            kinds,
            [["start", "iteration"], ["stop", "iteration"], ["done", "0"]]
            + [["start", "iteration"], ["stop", "iteration"], ["done", "1"]],
        )

    def test_the_interval_uses_the_monotonic_clock(self):
        lines = self.serve("run 0\n", lambda: time.sleep(0.05))
        start = int(lines[1].split()[2])
        stop = int(lines[2].split()[2])
        self.assertGreaterEqual(stop - start, 45_000_000)
        self.assertLess(stop - start, 1_000_000_000)

    def test_an_exception_is_a_failed_iteration_and_the_loop_goes_on(self):
        def function():
            raise ValueError("bad\ninput")

        lines = self.serve("run 0\nrun 1\n", function)
        self.assertEqual(lines[1:], ["fail 0 ValueError: bad input", "fail 1 ValueError: bad input"])

    def test_without_perfratio_the_function_runs_once(self):
        calls = []
        env = dict(os.environ)
        os.environ.pop(pr.PERSISTENT_ENV, None)
        try:
            pr.serve(lambda: calls.append(1))
        finally:
            os.environ.clear()
            os.environ.update(env)
        self.assertEqual(calls, [1])


if __name__ == "__main__":
    unittest.main()
