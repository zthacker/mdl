"""Reference values for mdl-testkit's oracle, from libraries outside this
repository: SciPy for splines and tables, pint for unit conversions. The
oracle's own arithmetic is checked against them, so it doesn't rest only
on agreeing with code written by the same hands.

Regenerate with a throwaway environment:

    python3 -m venv /tmp/mdl-refs
    /tmp/mdl-refs/bin/pip install scipy numpy pint
    /tmp/mdl-refs/bin/python crates/mdl-testkit/reference/generate.py

It rewrites crates/mdl-testkit/src/layout/reference.rs and runs rustfmt
on it.
"""

import pathlib
import subprocess

import numpy as np
import pint
from scipy.interpolate import CubicSpline, interp1d

OUT = pathlib.Path(__file__).resolve().parent.parent / "src" / "layout" / "reference.rs"

POINT_SETS = [
    [(0.0, 0.0), (1.0, 2.0)],
    [(-50.0, -50.0), (0.0, 0.0), (50.0, 60.0)],
    [(0.0, 10.0), (0.25, -3.5), (1.5, 7.0), (4.0, 7.5), (10.0, -100.0)],
    [(-1000.0, 1e6), (-999.0, -1e6), (3.0, 0.5), (1000.0, 2.0)],
]

# The codes the oracle converts between, by their pint names.
UNITS = {
    "mV": "millivolt",
    "V": "volt",
    "kV": "kilovolt",
    "mA": "milliampere",
    "A": "ampere",
    "K": "kelvin",
    "Cel": "degree_Celsius",
    "[degF]": "degree_Fahrenheit",
    "ms": "millisecond",
    "s": "second",
    "min": "minute",
    "h": "hour",
    "m": "meter",
    "km": "kilometer",
    "[ft_i]": "foot",
    "rad": "radian",
    "mrad": "milliradian",
    "deg": "degree",
}

GROUPS = [
    ["mV", "V", "kV"],
    ["mA", "A"],
    ["K", "Cel", "[degF]"],
    ["ms", "s", "min", "h"],
    ["m", "km", "[ft_i]"],
    ["rad", "mrad", "deg"],
]

VALUES = [0.0, 1.5, -40.0, 98.6, 1234.5678, -0.001, 3.0e5]


def inputs(points):
    """Each point, four values between the ends, and one past each end."""
    raws = [raw for raw, _ in points]
    first, last = raws[0], raws[-1]
    between = [first + (last - first) * f for f in (0.1, 0.333, 0.5, 0.9)]
    return raws + between + [first - 7.25, last + 3.5]


def spline(points):
    """SciPy's natural cubic spline, extrapolated straight along its end
    slopes, as spec/decoding.md 7.1 extrapolates it."""
    raws = np.array([raw for raw, _ in points])
    values = np.array([value for _, value in points])
    curve = CubicSpline(raws, values, bc_type="natural")
    out = []
    for x in inputs(points):
        if x < raws[0]:
            y = values[0] + (x - raws[0]) * curve(raws[0], 1)
        elif x > raws[-1]:
            y = values[-1] + (x - raws[-1]) * curve(raws[-1], 1)
        else:
            y = curve(x)
        out.append((x, float(y)))
    return out


def table(points):
    """SciPy's linear interpolation, extrapolating its end segments."""
    raws = [raw for raw, _ in points]
    values = [value for _, value in points]
    line = interp1d(raws, values, kind="linear", fill_value="extrapolate")
    return [(x, float(line(x))) for x in inputs(points)]


def conversions():
    registry = pint.UnitRegistry()
    out = []
    for group in GROUPS:
        for source in group:
            for target in group:
                if source == target:
                    continue
                for value in VALUES:
                    quantity = registry.Quantity(value, UNITS[source])
                    converted = quantity.to(UNITS[target]).magnitude
                    out.append((source, target, value, float(converted)))
    return out


def number(x):
    text = repr(float(x))
    return text if any(c in text for c in ".e") else text + ".0"


def pairs(values):
    return ", ".join(f"({number(a)}, {number(b)})" for a, b in values)


def curves(name, doc, compute):
    lines = [doc, f"pub(super) const {name}: &[Curve] = &["]
    for points in POINT_SETS:
        lines.append(
            f"    Curve {{ points: &[{pairs(points)}], values: &[{pairs(compute(points))}] }},"
        )
    lines.append("];")
    return "\n".join(lines)


SPLINE_DOC = (
    '/// Natural cubic splines (`CubicSpline`, `bc_type="natural"`), extrapolated\n'
    "/// straight along their end slopes."
)
TABLE_DOC = "/// Linear tables (`interp1d`), extrapolating their end segments."


def main():
    splines = curves("SPLINES", SPLINE_DOC, spline)
    tables = curves("TABLES", TABLE_DOC, table)
    units = "\n".join(
        f'    ("{source}", "{target}", {number(value)}, {number(converted)}),'
        for source, target, value, converted in conversions()
    )
    text = f"""//! Reference values from SciPy and pint, written by
//! `reference/generate.py` (SciPy {__import__("scipy").__version__}, NumPy
//! {np.__version__}, pint {pint.__version__}): regenerate, don't edit.

/// Calibration points, and (x, value) pairs through them.
pub(super) struct Curve {{
    pub points: &'static [(f64, f64)],
    pub values: &'static [(f64, f64)],
}}

{splines}

{tables}

/// (from, to, value, converted), by pint.
pub(super) const UNITS: &[(&str, &str, f64, f64)] = &[
{units}
];
"""
    OUT.write_text(text)
    subprocess.run(["rustfmt", "--edition", "2024", str(OUT)], check=True)


main()
