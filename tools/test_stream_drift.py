#!/usr/bin/env python3
"""Tests for stream-drift.py: python3 tools/test_stream_drift.py (needs numpy)."""
import importlib.util
import os
import stat
import sys
import tempfile
import time
import unittest

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("stream_drift", os.path.join(HERE, "stream-drift.py"))
sd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sd)


def synthetic(seconds, slope_ppm, lag=0.2, step_at=None, step_s=600, start_counter=1000):
    """Marker dicts for one connection: a marker each second, a counter on every tenth. The box clock steps
    forward `step_s` seconds at content second `step_at` (the counters jump, the audio goes on)."""
    ms = []
    for i in range(seconds):
        box = 1_800_000_000 + i
        if step_at is not None and i >= step_at:
            box += step_s
        # Receiver time: content time plus the receiver clock's drift against content time (box time before the step).
        arrival = 1_800_000_000 + i + lag + slope_ppm * 1e-6 * i
        counter = None
        if box % 10 == 0:
            counter = box // 10
        ms.append({"segment": 1, "onset_sample": i * sd.RATE + 0.0, "arrival": arrival, "counter": counter})
    return ms


class Fit(unittest.TestCase):
    def test_a_plain_stream_gives_its_slope(self):
        r = sd.analyse(synthetic(3000, 2.0))
        print("plain:", r["slope_ppm"], r["slopes_by_piece"])
        self.assertAlmostEqual(r["slope_ppm"], 2.0, delta=0.1)
        self.assertEqual(r["timeline_breaks_over_50ms"], [])

    def test_a_planned_clock_step_does_not_move_the_slope(self):
        ms = synthetic(3000, 2.0, step_at=1500)
        r = sd.analyse(ms)
        print("step:", r["slope_ppm"], r["slopes_by_piece"], r["timeline_breaks_over_50ms"])
        self.assertEqual(len(r["slopes_by_piece"]), 2)
        self.assertAlmostEqual(r["slope_ppm"], 2.0, delta=0.1)
        for p in r["slopes_by_piece"]:
            self.assertAlmostEqual(p["slope_ppm"], 2.0, delta=0.2)
        self.assertEqual(len(r["timeline_breaks_over_50ms"]), 1)
        self.assertFalse(r["pass"], "a break is still reported and still fails the run")


FAKE = """#!/bin/sh
# fake ffmpeg: the first $FAIL_FIRST invocations exit 1 at once (a refused connection); later ones send a
# second of silence as f32le and exit (the stream ending).
n=$(cat "$COUNT" 2>/dev/null || echo 0); n=$((n + 1)); echo $n > "$COUNT"
[ "$n" -le "$FAIL_FIRST" ] && exit 1
head -c 192000 /dev/zero
exit 0
"""


class Reconnect(unittest.TestCase):
    def test_refused_connections_and_dropped_streams_are_retried_with_backoff(self):
        with tempfile.TemporaryDirectory() as d:
            fake = os.path.join(d, "ffmpeg")
            open(fake, "w").write(FAKE)
            os.chmod(fake, os.stat(fake).st_mode | stat.S_IXUSR)
            os.environ["COUNT"] = os.path.join(d, "count")
            os.environ["FAIL_FIRST"] = "3"
            sd.FFMPEG = fake
            sd.BACKOFF_START, sd.BACKOFF_MAX = 0.05, 0.2
            an, extra = sd.Analyzer(), {}
            t0 = time.time()
            sd.live("http://127.0.0.1:1/none", 2.0, 0, an, extra)  # must return, not raise or exit
            n = int(open(os.environ["COUNT"]).read())
            print(f"connection attempts {n}, reconnects {extra['reconnects']}, segments {an.segment}, "
                  f"{time.time() - t0:.1f} s")
            self.assertGreaterEqual(n, 6)  # three refused, then successes that end and are retried
            self.assertIn(extra["reconnects"], (n, n - 1))  # the last attempt may be cut off by the time limit
            self.assertGreaterEqual(time.time() - t0, 1.9)  # it kept listening until the time was up

    def test_the_wait_doubles_to_its_cap(self):
        with tempfile.TemporaryDirectory() as d:
            fake = os.path.join(d, "ffmpeg")
            open(fake, "w").write(FAKE)
            os.chmod(fake, os.stat(fake).st_mode | stat.S_IXUSR)
            os.environ["COUNT"] = os.path.join(d, "count")
            os.environ["FAIL_FIRST"] = "1000"
            sd.FFMPEG = fake
            sd.BACKOFF_START, sd.BACKOFF_MAX = 0.1, 0.4
            extra = {}
            sd.live("x", 1.5, 0, sd.Analyzer(), extra)
            n = int(open(os.environ["COUNT"]).read())
            # waits 0.1 0.2 0.4 0.4 ...: attempts at 0, .1, .3, .7, 1.1, 1.5
            print("attempts in 1.5 s with doubling to 0.4 s:", n)
            self.assertTrue(4 <= n <= 7, n)


class RawLog(unittest.TestCase):
    def test_settled_markers_are_appended_once(self):
        with tempfile.TemporaryDirectory() as d:
            sd.RAW_LOG = os.path.join(d, "raw.csv")
            an = sd.Analyzer()
            an.markers = [{"segment": 1, "arrival": 100.0 + i, "onset_sample": 5.0, "counter": i} for i in range(5)]
            extra = {}
            sd._append_raw(an, extra, 200.0)
            sd._append_raw(an, extra, 200.0)
            lines = open(sd.RAW_LOG).read().splitlines()
            self.assertEqual(len(lines), 6)
            sd.RAW_LOG = None


if __name__ == "__main__":
    unittest.main(verbosity=2)
