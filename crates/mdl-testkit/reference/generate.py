"""Reference values for mdl-testkit's oracle, from libraries outside this
repository: SciPy for splines and tables, pint for unit conversions,
astropy for time scales and leap seconds. The
oracle's own arithmetic is checked against them, so it doesn't rest only
on agreeing with code written by the same hands.

Regenerate with a throwaway environment:

    python3 -m venv /tmp/mdl-refs
    /tmp/mdl-refs/bin/pip install scipy numpy pint astropy
    /tmp/mdl-refs/bin/python crates/mdl-testkit/reference/generate.py

It rewrites crates/mdl-testkit/src/layout/reference.rs and runs rustfmt
on it.
"""

import calendar
import datetime
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


def unix_nanos(text):
    """An astropy UTC ISO string with nine fraction digits as Unix
    nanoseconds; a second 60 (inside a leap second) is the last nanosecond
    before the leap, as spec/decoding.md 8.1 reads it."""
    day, clock = text.split(" ")
    hour, minute, second = clock.split(":")
    whole, fraction = second.split(".")
    leap = whole == "60"
    stamp = datetime.datetime.fromisoformat(f"{day}T{hour}:{minute}:{'59' if leap else whole}")
    seconds = calendar.timegm(stamp.timetuple())
    if leap:
        return seconds * 10**9 + 999_999_999
    return seconds * 10**9 + int(fraction.ljust(9, "0"))


def label_nanos(stamp):
    """A naive datetime's wall clock as a label: nanoseconds since 1970,
    every day 86,400 s."""
    return calendar.timegm(stamp.timetuple()) * 10**9 + stamp.microsecond * 1000


def times():
    """(scale, label, UTC instant) by astropy: TAI labels around every leap
    second from 1972-07-01 on (inside it too), GPS seconds at the anchors
    and week rollovers, and TT at J2000 and later."""
    from astropy.time import Time
    from astropy.utils import iers

    iers.conf.auto_download = False
    out = []
    leaps = [
        (1972, 7, 1, 11), (1973, 1, 1, 12), (1974, 1, 1, 13), (1975, 1, 1, 14),
        (1976, 1, 1, 15), (1977, 1, 1, 16), (1978, 1, 1, 17), (1979, 1, 1, 18),
        (1980, 1, 1, 19), (1981, 7, 1, 20), (1982, 7, 1, 21), (1983, 7, 1, 22),
        (1985, 7, 1, 23), (1988, 1, 1, 24), (1990, 1, 1, 25), (1991, 1, 1, 26),
        (1992, 7, 1, 27), (1993, 7, 1, 28), (1994, 7, 1, 29), (1996, 1, 1, 30),
        (1997, 7, 1, 31), (1999, 1, 1, 32), (2006, 1, 1, 33), (2009, 1, 1, 34),
        (2012, 7, 1, 35), (2015, 7, 1, 36), (2017, 1, 1, 37),
    ]
    for year, month, day, offset in leaps:
        start = datetime.datetime(year, month, day)
        for delta in (-1.5, -0.25, 0.0, 0.5, 0.999, 1.0, 1.25, 3600.0):
            stamp = start + datetime.timedelta(seconds=offset - 1 + delta)
            utc = Time(stamp.isoformat(), scale="tai").utc
            utc.precision = 9
            out.append(("tai", label_nanos(stamp), unix_nanos(utc.iso)))
    epoch = datetime.datetime(1980, 1, 6)
    for seconds in (0, 10**9, 1024 * 604800, 2048 * 604800, 1167264017, 1483228817.5):
        utc = Time(seconds, format="gps").utc
        utc.precision = 9
        stamp = epoch + datetime.timedelta(seconds=seconds)
        out.append(("gps", label_nanos(stamp), unix_nanos(utc.iso)))
    for text in ("2000-01-01T12:00:00", "2016-12-31T23:59:59.5", "2026-06-01T12:00:00.25"):
        utc = Time(text, scale="tt").utc
        utc.precision = 9
        out.append(("tt", label_nanos(datetime.datetime.fromisoformat(text)), unix_nanos(utc.iso)))
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
    instants = "\n".join(
        f'    ("{scale}", {label}, {utc}),' for scale, label, utc in times()
    )
    units = "\n".join(
        f'    ("{source}", "{target}", {number(value)}, {number(converted)}),'
        for source, target, value, converted in conversions()
    )
    text = f"""//! Reference values from SciPy, pint and astropy, written by
//! `reference/generate.py` (SciPy {__import__("scipy").__version__}, NumPy
//! {np.__version__}, pint {pint.__version__}, astropy
//! {__import__("astropy").__version__}): regenerate, don't edit.

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

/// (scale, label, UTC instant), by astropy: a label is nanoseconds since
/// 1970-01-01T00:00:00 on its scale, every day 86,400 s; an instant is Unix
/// nanoseconds, a time inside a leap second the last before it.
pub(super) const TIMES: &[(&str, i128, i128)] = &[
{instants}
];
"""
    OUT.write_text(text)
    subprocess.run(["rustfmt", "--edition", "2024", str(OUT)], check=True)


main()
