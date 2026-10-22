#!/usr/bin/env python3
"""Measure the clock drift and the continuity of a Hotclock stream (architecture 1.3a, M2a acceptance 5).

The station runs with `hotclock station <name> --stream-test`: a 1 kHz tone, a 60 ms burst of 3 kHz on every
whole second of the box's CLOCK_REALTIME, and every 10 s a 24-bit counter (number of 10 s periods since the
epoch) as 24 bursts of 50 ms, 2 kHz = 1, 4 kHz = 0, starting 100 ms after that second's marker.

Two ways to listen:

  stream-drift.py --url http://radiobox:8301/stream/program.ogg --seconds 259200
      runs ffmpeg and timestamps the audio as it arrives (reconnects after a box restart). The arrival time of a
      sample is the time its chunk was read minus the time the rest of the chunk takes to play.
  stream-drift.py --wav obs-recording.wav --start 1767225600.0
      reads a recording (OBS's local recording of the mix, converted to WAV); sample i is taken to be
      `start + i / rate`, where `start` is the recording's start in epoch seconds (the file's modification time
      minus its length, if nothing better is known).

It reports: the slope of (arrival time - box time) against box time in parts per million (the drift of the
receiver's clock against the box's), the intercept (the fixed lag), the counter's gaps and repeats, and any
break in the timeline longer than 50 ms. Pass: slope within +-5 ppm and no break a listener would notice.

Needs numpy. The marker is found to about 1 ms (1 ms analysis bins).
"""
import argparse
import json
import math
import os
import subprocess
import sys
import time

import numpy as np

RATE = 48000
BIN = RATE // 1000  # 48 samples: whole cycles of 1, 2, 3 and 4 kHz
THR = 0.05  # burst amplitude threshold (the bursts are 0.1 peak)
MARKER_BINS = 60
BITS = 24
BIT_BINS = 50
BITS_OFFSET_BINS = 100
KEEP_BINS = 6000  # 6 s of bin history


class Analyzer:
    """Streaming detector: 1 ms amplitude bins at 3, 2 and 4 kHz, markers, counters."""

    def __init__(self, rate=RATE):
        assert rate == RATE
        self.carry = np.zeros(0, dtype=np.float32)
        self.g = 0  # samples consumed (start of carry)
        self.base_bin = 0  # global index of amp[...][0]
        self.amp = {hz: np.zeros(0, dtype=np.float32) for hz in (3000, 2000, 4000)}
        self.scan = 0  # next global bin to look at for a rising edge
        self.markers = []  # dicts: onset_bin, arrival, segment, counter (or None)
        self.pending = []  # markers waiting for the bins their counter needs
        self.segment = 0
        self.arrival_of = None  # function(global_sample) -> epoch seconds
        self._tw = {hz: None for hz in self.amp}

    def new_segment(self):
        """A new connection: the sample clock restarts (the timeline is a new stream)."""
        self.segment += 1
        self.carry = np.zeros(0, dtype=np.float32)
        self.g = 0
        self.base_bin = 0
        self.amp = {hz: np.zeros(0, dtype=np.float32) for hz in self.amp}
        self.scan = 0
        self.pending = []

    def _bins(self, x, hz):
        n = len(x) // BIN
        k = np.arange(n * BIN)
        z = x[: n * BIN] * np.exp(-2j * math.pi * hz * k / RATE)
        return (np.abs(z.reshape(n, BIN).sum(axis=1)) / BIN * 2).astype(np.float32)

    def feed(self, samples, arrival_end=None, start_epoch=None):
        """`samples`: mono float32. `arrival_end`: epoch time the last sample arrived (live mode), or
        `start_epoch`: the epoch time of sample 0 of this segment (recording mode)."""
        x = np.concatenate([self.carry, samples])
        n = len(x) // BIN
        if n == 0:
            self.carry = x
            return
        body = x[: n * BIN]
        self.carry = x[n * BIN :]
        end_g = self.g + n * BIN + len(self.carry)  # global index after the last sample received
        if arrival_end is not None:
            self.arrival_of = lambda g, e=end_g, t=arrival_end: t - (e - g) / RATE
        else:
            self.arrival_of = lambda g, s=start_epoch: s + g / RATE
        for hz in self.amp:
            self.amp[hz] = np.concatenate([self.amp[hz], self._bins(body, hz)])
        self.g += n * BIN
        self._detect()
        self._decode()
        drop = len(self.amp[3000]) - KEEP_BINS
        if drop > 0:
            for hz in self.amp:
                self.amp[hz] = self.amp[hz][drop:]
            self.base_bin += drop

    def _detect(self):
        a = self.amp[3000]
        top = self.base_bin + len(a)
        i = max(self.scan, self.base_bin + 1)
        while i + MARKER_BINS < top:
            j = i - self.base_bin
            if a[j] >= THR and a[j - 1] < THR:
                # A marker: the burst holds above the threshold for most of 60 ms.
                if (a[j + 8 : j + MARKER_BINS - 8] >= THR).all():
                    onset = self.base_bin + j  # first bin at or above the threshold
                    # Sub-bin edge: the 2 ms raised-cosine ramp passes THR somewhere in this bin.
                    plateau = float(a[j + 20 : j + 40].mean())
                    frac = 0.0
                    if a[j - 1] < THR:
                        lo, hi = float(a[j - 1]), float(a[j])
                        frac = 1.0 - (THR - lo) / (hi - lo) if hi > lo else 0.0
                    onset_s = (onset - frac) * BIN
                    m = {
                        "segment": self.segment,
                        "onset_sample": onset_s,
                        "onset_bin": onset,
                        "arrival": self.arrival_of(onset_s),
                        "plateau": plateau,
                        "counter": None,
                    }
                    self.markers.append(m)
                    self.pending.append(m)
                    i += MARKER_BINS
                    continue
            i += 1
        self.scan = i

    def _decode(self):
        top = self.base_bin + len(self.amp[3000])
        keep = []
        for m in self.pending:
            need = m["onset_bin"] + BITS_OFFSET_BINS + BITS * BIT_BINS + 10
            if need > top:
                keep.append(m)
                continue
            if m["onset_bin"] - 5 < self.base_bin:
                continue
            m["counter"] = self._counter_at(m["onset_bin"])
        self.pending = keep

    def _counter_at(self, onset):
        a2, a4 = self.amp[2000], self.amp[4000]
        value = 0
        for k in range(BITS):
            c = onset + BITS_OFFSET_BINS + k * BIT_BINS + BIT_BINS // 2 - self.base_bin
            w2 = float(a2[c - 12 : c + 12].mean())
            w4 = float(a4[c - 12 : c + 12].mean())
            if max(w2, w4) < 0.04:
                return None
            value = (value << 1) | (1 if w2 > w4 else 0)
        return value


def analyse(markers, verbose=True):
    """Number the markers, then fit arrival - box time. Returns a dict."""
    out = {"markers": len(markers)}
    anchors = [m for m in markers if m["counter"] is not None]
    out["counters"] = len(anchors)
    if len(anchors) < 2:
        out["error"] = "fewer than two counters decoded"
        return out
    # A timeline break is where the counter's step (box time) and the audio's content time disagree by more than
    # 50 ms: a planned clock step on the box, a dropped stretch of audio. The timeline on either side is its own
    # piece: box time is numbered and the slope fitted per piece, so a break does not move the slope.
    breaks = []
    skips = []
    repeats = []
    cuts = {}  # segment -> onset_samples (of the later counter) where a new piece starts
    for seg in sorted({m["segment"] for m in markers}):
        an = [m for m in markers if m["segment"] == seg and m["counter"] is not None]
        cuts[seg] = []
        for p, q in zip(an, an[1:]):
            step = q["counter"] - p["counter"]
            content = (q["onset_sample"] - p["onset_sample"]) / RATE
            if step <= 0:
                repeats.append((p["counter"], q["counter"]))
            elif step > 1:
                skips.append((p["counter"], q["counter"], (step - 1) * 10.0))
            if abs(content - step * 10.0) > 0.05:
                breaks.append((p["counter"], q["counter"], content - step * 10.0))
                cuts[seg].append(q["onset_sample"])
    for m in markers:
        m["piece"] = (m["segment"], sum(1 for c in cuts.get(m["segment"], []) if m["onset_sample"] >= c))
    # Box time (whole second) of every marker: from the counters of its own piece, by content time.
    for piece in sorted({m["piece"] for m in markers}):
        sm = [m for m in markers if m["piece"] == piece]
        an = [m for m in sm if m["counter"] is not None]
        if not an:
            for m in sm:
                m["S"] = None
            continue
        for m in sm:
            nearest = min(an, key=lambda c: abs(c["onset_sample"] - m["onset_sample"]))
            dt = (m["onset_sample"] - nearest["onset_sample"]) / RATE
            # The counter is 24 bits of 10 s periods (5.3 years): pick the wrap that puts the box time
            # nearest the time the marker arrived.
            span = (1 << BITS) * 10.0
            base = nearest["counter"] * 10.0
            wraps = round((nearest["arrival"] - base) / span)
            m["S"] = base + wraps * span + round(dt)
    used = [m for m in markers if m.get("S") is not None]
    out["counter_skips"] = skips
    out["counter_repeats"] = repeats
    out["timeline_breaks_over_50ms"] = [
        {"from": a, "to": b, "excess_s": round(e, 3)} for a, b, e in breaks
    ]
    # Marker spacing within a segment: a missing second is a gap of that length.
    missing = 0
    for seg in sorted({m["segment"] for m in used}):
        s = sorted(m["S"] for m in used if m["segment"] == seg)
        missing += int(s[-1] - s[0] + 1) - len(set(s))
    out["markers_missing_in_span"] = missing
    # Drift: pooled within-piece regression of (arrival - S) on S, after the connect burst. A piece is the
    # stretch of one connection between two timeline breaks.
    num = den = 0.0
    xs, ys, segs = [], [], []
    per_piece = []
    for piece in sorted({m["piece"] for m in used}):
        sm = sorted((m for m in used if m["piece"] == piece), key=lambda m: m["S"])
        sm = [m for m in sm if m["S"] - sm[0]["S"] >= 10]  # drop the connect burst's first 10 s
        if len(sm) < 3:
            continue
        seg = piece[0]
        S = np.array([m["S"] for m in sm], dtype=float)
        d = np.array([m["arrival"] - m["S"] for m in sm])
        S0, d0 = S.mean(), d.mean()
        pn = float(((S - S0) * (d - d0)).sum())
        pd = float(((S - S0) ** 2).sum())
        if pd > 0:
            per_piece.append({"connection": piece[0], "piece": piece[1], "points": len(sm),
                              "span_s": round(float(S[-1] - S[0]), 1), "slope_ppm": round(pn / pd * 1e6, 3)})
        num += pn
        den += pd
        xs.extend(S - S0)
        ys.extend(d - d0)
        segs.extend([seg] * len(S))
    if den == 0:
        out["error"] = "not enough markers to fit"
        return out
    slope = num / den
    res = np.array(ys) - slope * np.array(xs)
    # Reject gross outliers (a late chunk, a gap) and refit once.
    keep = np.abs(res - np.median(res)) < 0.25
    if keep.sum() >= 3 and (~keep).any():
        x = np.array(xs)[keep]
        y = np.array(ys)[keep]
        slope = float((x * y).sum() / (x * x).sum())
        res = y - slope * x
    # Delay jitter only ever adds (a late read, a burst of pages), so the lowest delay in each minute follows the
    # clocks more closely than the mean does. Reported beside the least-squares slope.
    xa, ya = np.array(xs)[keep], np.array(ys)[keep]
    if len(xa) >= 120:
        b = np.floor((xa - xa.min()) / 60.0).astype(int)
        bx = np.array([xa[b == k].mean() for k in np.unique(b)])
        by = np.array([ya[b == k].min() for k in np.unique(b)])
        if len(bx) >= 3:
            out["slope_min_envelope_ppm"] = round(float(np.polyfit(bx, by, 1)[0]) * 1e6, 3)
    out["slopes_by_piece"] = per_piece
    out["fit_points"] = int(len(res))
    out["outliers_rejected"] = int((~keep).sum())
    out["slope_ppm"] = round(slope * 1e6, 3)
    out["residual_rms_ms"] = round(float(res.std()) * 1e3, 2)
    out["residual_max_ms"] = round(float(np.abs(res).max()) * 1e3, 2)
    out["span_s"] = round(max(m["S"] for m in used) - min(m["S"] for m in used), 1)
    for seg in sorted({m["segment"] for m in used}):
        sm = [m for m in used if m["segment"] == seg]
        out.setdefault("lag_s_by_connection", []).append(
            round(float(np.median([m["arrival"] - m["S"] for m in sm])), 3)
        )
    verdict = abs(slope * 1e6) <= 5.0 and not out["timeline_breaks_over_50ms"] and not repeats
    out["pass"] = bool(verdict)
    return out


RAW_LOG = None  # set by main(): markers appended as they settle, so a crash or reboot loses at most one status interval
BACKOFF_START = 1.0  # seconds before the first reconnect; doubles per failed attempt
BACKOFF_MAX = 60.0
READ_STALL_S = 30.0  # no bytes for this long on an open connection: drop it and reconnect
FFMPEG = "ffmpeg"


def _append_raw(an, extra, now):
    """Append settled markers (older than 60 s, so their counter is filled) to RAW_LOG."""
    if not RAW_LOG:
        return
    i = extra.get("raw_written", 0)
    j = i
    while j < len(an.markers) and an.markers[j]["arrival"] < now - 60:
        j += 1
    if j > i:
        new = not os.path.exists(RAW_LOG)
        with open(RAW_LOG, "a") as f:
            if new:
                f.write("segment,arrival,onset_sample,counter\n")
            for m in an.markers[i:j]:
                f.write(f"{m['segment']},{m['arrival']:.6f},{m['onset_sample']:.1f},{m['counter']}\n")
        extra["raw_written"] = j


def _event(**kw):
    print(json.dumps({"t": round(time.time()), **kw}), flush=True)


def _one_connection(url, t_end, status_every, an, extra, last_status):
    """Listen on one connection until it ends, stalls or the time is up. Returns (got_audio, last_status)."""
    import select

    cmd = [FFMPEG, "-nostdin", "-loglevel", "error", "-i", url, "-vn", "-f", "f32le",
           "-ac", "1", "-ar", str(RATE), "-flush_packets", "1", "-"]
    p = subprocess.Popen(cmd, stdout=subprocess.PIPE)
    try:
        an.new_segment()
        fd = p.stdout.fileno()
        pending = b""
        got = False
        while time.time() < t_end:
            ready, _, _ = select.select([fd], [], [], min(READ_STALL_S, max(0.1, t_end - time.time())))
            if not ready:
                if time.time() >= t_end:
                    break
                _event(stalled_s=READ_STALL_S)
                break
            chunk = os.read(fd, 1 << 16)
            now = time.time()
            if not chunk:
                break
            got = True
            pending += chunk
            n = len(pending) // 4
            if n:
                an.feed(np.frombuffer(pending[: n * 4], dtype="<f4"), arrival_end=now)
                pending = pending[n * 4 :]
            if status_every and now - last_status >= status_every:
                last_status = now
                _append_raw(an, extra, now)
                # Counts only. The whole-history analyse() here stalled this reader past the server's
                # listener_lag_s once the history passed about 11 h, and the station dropped the listener as
                # `slow` every hour (the 2026-10-01 Box A soak). The fit runs once, at the end.
                print(json.dumps({"t": round(now), "markers": len(an.markers),
                                  "counters": sum(1 for m in an.markers if m["counter"] is not None),
                                  "reconnects": extra.get("reconnects", 0)}),
                      flush=True)
        return got, last_status
    finally:
        p.kill()
        p.wait()


def live(url, seconds, status_every, an, extra):
    """Listen for `seconds`, reconnecting whenever the stream drops, refuses a connection or stalls (a station
    kill -9 refused one connection on 2026-10-05 and the listener used to exit, losing an hour of data). The wait
    before a reconnect starts at BACKOFF_START and doubles on each attempt that got no audio, up to BACKOFF_MAX;
    an attempt that got audio starts it over."""
    t_end = time.time() + seconds
    last = time.time()
    fails = 0  # attempts in a row that got no audio
    while time.time() < t_end:
        try:
            got, last = _one_connection(url, t_end, status_every, an, extra, last)
            err = None
        except FileNotFoundError:
            raise SystemExit(f"{FFMPEG}: not found")
        except Exception as e:  # keep listening: one bad connection must not end the run
            got, err = False, repr(e)
        if time.time() >= t_end:
            break
        extra["reconnects"] = extra.get("reconnects", 0) + 1
        wait = min(BACKOFF_START * 2 ** fails, BACKOFF_MAX) if not got else BACKOFF_START
        fails = 0 if got else fails + 1
        _event(reconnect=extra["reconnects"], audio=got, wait_s=round(wait, 1), **({"error": err} if err else {}))
        time.sleep(min(wait, max(0.0, t_end - time.time())))


def recording(path, start, an):
    import wave

    with wave.open(path, "rb") as w:
        assert w.getframerate() == RATE, "the recording must be 48 kHz"
        ch, width = w.getnchannels(), w.getsampwidth()
        an.new_segment()
        pos = 0
        while True:
            raw = w.readframes(RATE * 10)
            if not raw:
                break
            if width == 2:
                a = np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0
            elif width == 4:
                a = np.frombuffer(raw, dtype="<i4").astype(np.float32) / 2147483648.0
            else:
                raise SystemExit("use 16- or 32-bit PCM")
            a = a.reshape(-1, ch)[:, 0]
            an.feed(a, start_epoch=start + (pos - an.g) / RATE if False else start)
            pos += len(a)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--url")
    src.add_argument("--wav")
    ap.add_argument("--start", type=float, help="epoch seconds of the recording's first sample (with --wav)")
    ap.add_argument("--seconds", type=float, default=600, help="how long to listen (with --url)")
    ap.add_argument("--status-every", type=float, default=0)
    ap.add_argument("--csv")
    ap.add_argument("--raw-log", help="append settled markers here at every status tick (survives a crash)")
    a = ap.parse_args()
    an = Analyzer()
    extra = {}
    if a.url:
        global RAW_LOG
        RAW_LOG = a.raw_log
        import signal

        def _stop(signum, frame):
            raise KeyboardInterrupt

        signal.signal(signal.SIGTERM, _stop)
        try:
            live(a.url, a.seconds, a.status_every, an, extra)
        except KeyboardInterrupt:
            extra["stopped_early"] = True
            _event(stopped="signal")
        _append_raw(an, extra, time.time() + 120)
    else:
        if a.start is None:
            ap.error("--wav needs --start")
        recording(a.wav, a.start, an)
    res = analyse(an.markers)
    res.update({k: v for k, v in extra.items() if not k.startswith("_")})
    print(json.dumps(res, indent=2))
    if a.csv:
        with open(a.csv, "w") as f:
            f.write("segment,S,arrival,onset_sample,counter\n")
            for m in an.markers:
                f.write(f"{m['segment']},{m.get('S')},{m['arrival']:.6f},{m['onset_sample']:.1f},{m['counter']}\n")
    return 0 if res.get("pass") else 1


if __name__ == "__main__":
    sys.exit(main())
