#!/usr/bin/env python3
"""Print the Planckian locus table of crates/common/src/night.rs (PLANCK_XY).

Each row is the CIE 1931 xy chromaticity of a black body at T kelvin, from Planck's law (CODATA 2018 constants)
weighted by the CIE 1931 2-degree standard observer at 1 nm from 360 to 830 nm. The observer is read from the
colour-science package's dataset file (the CIE's published table):

    curl -sSO https://raw.githubusercontent.com/colour-science/colour/develop/colour/colorimetry/datasets/cmfs.py
    python3 -I tools/blackbody-table.py cmfs.py

Check: 2856 K gives x 0.44754, y 0.40743 (CIE illuminant A is 0.44757, 0.40745).
"""
import math
import re
import sys

src = open(sys.argv[1], encoding="utf-8").read()
i = src.index('"CIE 1931 2 Degree Standard Observer": {')
j = src.index("},", i)
cmf = {}
for m in re.finditer(r"(\d+): \(([-\d.e]+), ([-\d.e]+), ([-\d.e]+)\)", src[i:j]):
    cmf[int(m.group(1))] = tuple(float(m.group(k)) for k in (2, 3, 4))
assert min(cmf) == 360 and max(cmf) == 830 and len(cmf) == 471, "unexpected observer table"

H, C, K = 6.62607015e-34, 2.99792458e8, 1.380649e-23
C2 = H * C / K


def xy(t):
    X = Y = Z = 0.0
    for nm, (a, b, c) in cmf.items():
        lam = nm * 1e-9
        m = 1.0 / (lam**5 * (math.exp(C2 / (lam * t)) - 1.0))
        X += m * a
        Y += m * b
        Z += m * c
    s = X + Y + Z
    return X / s, Y / s


for t in range(1000, 10001, 100):
    x, y = xy(t)
    print(f"    ({t}, {x:.5f}, {y:.5f}),")
