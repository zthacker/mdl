//! Units of measure: the subset of UCUM's case-sensitive codes MDL knows,
//! their dimensions, and converting values between compatible units, as
//! `spec/units.md` defines them.
//!
//! A unit is a product of components: atoms (`m`, `V`, `[ft_i]`), each with
//! an optional prefix (`k`, `m`, `u`) and integer exponent (`s-2`), powers
//! of ten (`10*3`), integers, parenthesized terms, and annotations
//! (`{counts}`), joined by `.` and `/`. Its *factor* says how many base
//! units one of it is; values convert as `(v × from) / to`, in binary64.

use std::fmt;

/// Exponents of UCUM's seven base dimensions: length (m), time (s), mass
/// (g), plane angle (rad), temperature (K), charge (C), luminous intensity
/// (cd).
type Dimension = [i32; 7];

const NONE: Dimension = [0; 7];

// The base dimensions' places in a `Dimension`.
const LENGTH: usize = 0;
const TIME: usize = 1;
const MASS: usize = 2;
const ANGLE: usize = 3;
const TEMPERATURE: usize = 4;
const CHARGE: usize = 5;
const LUMINOUS: usize = 6;

/// A dimension from (base, exponent) pairs.
const fn dim(exponents: &[(usize, i32)]) -> Dimension {
    let mut dimension = NONE;
    let mut i = 0;
    while i < exponents.len() {
        let (base, exponent) = exponents[i];
        dimension[base] += exponent;
        i += 1;
    }
    dimension
}

/// A unit, parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct Unit {
    dimension: Dimension,
    /// Base units in one of this unit.
    factor: f64,
    /// Celsius or Fahrenheit, which convert with an offset.
    scale: Option<Temperature>,
    /// Annotations, sorted: two units are compatible only with the same.
    annotations: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Temperature {
    Celsius,
    Fahrenheit,
}

/// Why a unit string isn't a unit MDL knows.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnitError {
    pub unit: String,
    pub reason: String,
}

impl fmt::Display for UnitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} isn't a unit MDL knows: {}", self.unit, self.reason)
    }
}

impl std::error::Error for UnitError {}

/// One atom: its code, dimension, factor, and whether it takes prefixes.
struct Atom {
    code: &'static str,
    dimension: Dimension,
    factor: f64,
    metric: bool,
}

const fn atom(code: &'static str, dimension: Dimension, factor: f64, metric: bool) -> Atom {
    Atom {
        code,
        dimension,
        factor,
        metric,
    }
}

/// Dimensions the atoms share.
const LEN: Dimension = dim(&[(LENGTH, 1)]);
const DURATION: Dimension = dim(&[(TIME, 1)]);
const RATE: Dimension = dim(&[(TIME, -1)]);
const MASSIVE: Dimension = dim(&[(MASS, 1)]);
const ANGULAR: Dimension = dim(&[(ANGLE, 1)]);
const VOLUME: Dimension = dim(&[(LENGTH, 3)]);
const FORCE: Dimension = dim(&[(MASS, 1), (LENGTH, 1), (TIME, -2)]);
const PRESSURE: Dimension = dim(&[(MASS, 1), (LENGTH, -1), (TIME, -2)]);
const ENERGY: Dimension = dim(&[(MASS, 1), (LENGTH, 2), (TIME, -2)]);
const DOSE: Dimension = dim(&[(LENGTH, 2), (TIME, -2)]);

/// The atoms MDL knows. Factors are in base units (the gram, not the
/// kilogram, is UCUM's base, so a newton is 1000); `spec/units.md` lists
/// them with their decimal values.
const ATOMS: &[Atom] = &[
    // Base units.
    atom("m", LEN, 1.0, true),
    atom("s", DURATION, 1.0, true),
    atom("g", MASSIVE, 1.0, true),
    atom("rad", ANGULAR, 1.0, true),
    atom("K", dim(&[(TEMPERATURE, 1)]), 1.0, true),
    atom("C", dim(&[(CHARGE, 1)]), 1.0, true),
    atom("cd", dim(&[(LUMINOUS, 1)]), 1.0, true),
    // Dimensionless.
    atom("%", NONE, 0.01, false),
    atom("[ppm]", NONE, 1e-6, false),
    atom("[ppb]", NONE, 1e-9, false),
    atom("mol", NONE, 6.0221367e23, true),
    atom("sr", dim(&[(ANGLE, 2)]), 1.0, true),
    // SI derived.
    atom("Hz", RATE, 1.0, true),
    atom("N", FORCE, 1000.0, true),
    atom("Pa", PRESSURE, 1000.0, true),
    atom("J", ENERGY, 1000.0, true),
    atom(
        "W",
        dim(&[(MASS, 1), (LENGTH, 2), (TIME, -3)]),
        1000.0,
        true,
    ),
    atom("A", dim(&[(CHARGE, 1), (TIME, -1)]), 1.0, true),
    atom(
        "V",
        dim(&[(MASS, 1), (LENGTH, 2), (TIME, -2), (CHARGE, -1)]),
        1000.0,
        true,
    ),
    atom(
        "F",
        dim(&[(CHARGE, 2), (TIME, 2), (MASS, -1), (LENGTH, -2)]),
        0.001,
        true,
    ),
    atom(
        "Ohm",
        dim(&[(MASS, 1), (LENGTH, 2), (TIME, -1), (CHARGE, -2)]),
        1000.0,
        true,
    ),
    atom(
        "S",
        dim(&[(CHARGE, 2), (TIME, 1), (MASS, -1), (LENGTH, -2)]),
        0.001,
        true,
    ),
    atom(
        "Wb",
        dim(&[(MASS, 1), (LENGTH, 2), (TIME, -1), (CHARGE, -1)]),
        1000.0,
        true,
    ),
    atom(
        "T",
        dim(&[(MASS, 1), (TIME, -1), (CHARGE, -1)]),
        1000.0,
        true,
    ),
    atom(
        "H",
        dim(&[(MASS, 1), (LENGTH, 2), (CHARGE, -2)]),
        1000.0,
        true,
    ),
    atom("lm", dim(&[(LUMINOUS, 1), (ANGLE, 2)]), 1.0, true),
    atom(
        "lx",
        dim(&[(LUMINOUS, 1), (ANGLE, 2), (LENGTH, -2)]),
        1.0,
        true,
    ),
    atom("Bq", RATE, 1.0, true),
    atom("Gy", DOSE, 1.0, true),
    atom("Sv", DOSE, 1.0, true),
    // Other metric units.
    atom("L", VOLUME, 0.001, true),
    atom("l", VOLUME, 0.001, true),
    atom("t", MASSIVE, 1e6, true),
    atom("bar", PRESSURE, 1e8, true),
    atom("eV", ENERGY, 1.60217733e-16, true),
    // Time and angle.
    atom("min", DURATION, 60.0, false),
    atom("h", DURATION, 3600.0, false),
    atom("d", DURATION, 86400.0, false),
    atom("wk", DURATION, 604800.0, false),
    atom("a", DURATION, 31557600.0, false),
    atom("deg", ANGULAR, 0.017453292519943295, false),
    atom("'", ANGULAR, 0.0002908882086657216, false),
    atom("''", ANGULAR, 4.84813681109536e-6, false),
    // Pressure and acceleration.
    atom("atm", PRESSURE, 101325000.0, false),
    atom("[g]", dim(&[(LENGTH, 1), (TIME, -2)]), 9.80665, false),
    // US customary.
    atom("[in_i]", LEN, 0.0254, false),
    atom("[ft_i]", LEN, 0.3048, false),
    atom("[yd_i]", LEN, 0.9144, false),
    atom("[mi_i]", LEN, 1609.344, false),
    atom("[nmi_i]", LEN, 1852.0, false),
    atom(
        "[kn_i]",
        dim(&[(LENGTH, 1), (TIME, -1)]),
        0.5144444444444445,
        false,
    ),
    atom("[lb_av]", MASSIVE, 453.59237, false),
    atom("[lbf_av]", FORCE, 4448.2216152605, false),
    atom("[psi]", PRESSURE, 6894757.293168361, false),
    atom("[gal_us]", VOLUME, 0.003785411784, false),
    // Data.
    atom("bit", NONE, 1.0, true),
    atom("By", NONE, 8.0, true),
    atom("Bd", RATE, 1.0, true),
];

/// UCUM's prefixes, longest code first so `da` is tried before `d`; the
/// binary ones (`Ki` is 1024) are for data sizes.
const PREFIXES: &[(&str, f64)] = &[
    ("da", 1e1),
    ("Ki", 1024.0),
    ("Mi", 1_048_576.0),
    ("Gi", 1_073_741_824.0),
    ("Ti", 1_099_511_627_776.0),
    ("Y", 1e24),
    ("Z", 1e21),
    ("E", 1e18),
    ("P", 1e15),
    ("T", 1e12),
    ("G", 1e9),
    ("M", 1e6),
    ("k", 1e3),
    ("h", 1e2),
    ("d", 1e-1),
    ("c", 1e-2),
    ("m", 1e-3),
    ("u", 1e-6),
    ("n", 1e-9),
    ("p", 1e-12),
    ("f", 1e-15),
    ("a", 1e-18),
    ("z", 1e-21),
    ("y", 1e-24),
];

impl Unit {
    /// Parses a unit code. Celsius (`Cel`) and Fahrenheit (`[degF]`)
    /// stand alone: they convert with an offset, which a product can't
    /// carry.
    pub fn parse(code: &str) -> Result<Unit, UnitError> {
        let error = |reason: String| UnitError {
            unit: code.to_string(),
            reason,
        };
        match code {
            "Cel" => return Ok(Unit::temperature(Temperature::Celsius)),
            "[degF]" => return Ok(Unit::temperature(Temperature::Fahrenheit)),
            "" => return Err(error("it's empty; leave the unit out instead".to_string())),
            _ => {}
        }
        let mut parser = Parser {
            text: code.as_bytes(),
            at: 0,
            depth: 0,
        };
        let mut unit = parser.term().map_err(error)?;
        if parser.at != parser.text.len() {
            return Err(error(format!("unexpected text at byte {}", parser.at)));
        }
        if !unit.factor.is_normal() {
            return Err(error(format!(
                "its factor, {}, isn't a normal double (it's zero, subnormal or infinite)",
                unit.factor
            )));
        }
        unit.annotations.sort();
        Ok(unit)
    }

    fn temperature(scale: Temperature) -> Unit {
        Unit {
            dimension: dim(&[(TEMPERATURE, 1)]),
            factor: 1.0,
            scale: Some(scale),
            annotations: Vec::new(),
        }
    }

    fn one() -> Unit {
        Unit {
            dimension: NONE,
            factor: 1.0,
            scale: None,
            annotations: Vec::new(),
        }
    }

    /// Whether values convert between the two: the same dimension and the
    /// same annotations.
    pub fn is_compatible(&self, other: &Unit) -> bool {
        self.dimension == other.dimension && self.annotations == other.annotations
    }

    /// A value in this unit, in `to`, if they're compatible:
    /// `(v × factor) / to.factor`, or through kelvin for Celsius and
    /// Fahrenheit. The same unit (same dimension, factor, scale and
    /// annotations) converts nothing.
    pub fn convert(&self, value: f64, to: &Unit) -> Option<f64> {
        if self == to {
            return Some(value);
        }
        if !self.is_compatible(to) {
            return None;
        }
        Some(match (self.scale, to.scale) {
            (None, None) => value * self.factor / to.factor,
            _ => {
                let kelvin = match self.scale {
                    Some(Temperature::Celsius) => value + 273.15,
                    Some(Temperature::Fahrenheit) => (value + 459.67) * 5.0 / 9.0,
                    None => value * self.factor,
                };
                match to.scale {
                    Some(Temperature::Celsius) => kelvin - 273.15,
                    Some(Temperature::Fahrenheit) => kelvin * 9.0 / 5.0 - 459.67,
                    None => kelvin / to.factor,
                }
            }
        })
    }

    fn times(mut self, other: Unit, exponent: i32) -> Unit {
        for (mine, theirs) in self.dimension.iter_mut().zip(other.dimension) {
            *mine += theirs * exponent;
        }
        self.factor = if exponent < 0 {
            self.factor / power(other.factor, -exponent)
        } else {
            self.factor * power(other.factor, exponent)
        };
        self.annotations.extend(other.annotations);
        self
    }
}

/// `x` multiplied into 1, `n` times.
fn power(x: f64, n: i32) -> f64 {
    (0..n).fold(1.0, |product, _| product * x)
}

/// A value converted between two unit codes, which validation has checked
/// are compatible: the value itself when they're the same code or either is
/// missing.
pub(crate) fn convert_between(value: f64, from: Option<&str>, to: Option<&str>) -> Option<f64> {
    match (from, to) {
        (Some(from), Some(to)) if from != to => Unit::parse(from)
            .ok()?
            .convert(value, &Unit::parse(to).ok()?),
        _ => Some(value),
    }
}

/// A recursive-descent parser over a unit code's bytes.
struct Parser<'a> {
    text: &'a [u8],
    at: usize,
    depth: usize,
}

/// Parentheses nest at most this deep.
const MAX_DEPTH: usize = 8;

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    /// `term := ['/'] component (('.' | '/') component)*`
    fn term(&mut self) -> Result<Unit, String> {
        let mut unit = Unit::one();
        let mut exponent = 1;
        if self.peek() == Some(b'/') {
            self.at += 1;
            exponent = -1;
        }
        loop {
            let component = self.component()?;
            unit = unit.times(component, exponent);
            match self.peek() {
                Some(b'.') => exponent = 1,
                Some(b'/') => exponent = -1,
                _ => return Ok(unit),
            }
            self.at += 1;
        }
    }

    /// An atom with its prefix, exponent and annotation; a power of ten or
    /// an integer; a parenthesized term; or an annotation alone.
    fn component(&mut self) -> Result<Unit, String> {
        let unit = match self.peek() {
            Some(b'(') => {
                self.at += 1;
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(format!("parentheses nest more than {MAX_DEPTH} deep"));
                }
                let inner = self.term()?;
                self.depth -= 1;
                if self.peek() != Some(b')') {
                    return Err(format!("no ')' at byte {}", self.at));
                }
                self.at += 1;
                inner
            }
            Some(b'{') => return self.annotation(Unit::one()),
            Some(c) if c.is_ascii_digit() => self.number()?,
            Some(_) => {
                let symbol = self.symbol()?;
                let exponent = self.exponent()?;
                Unit::one().times(symbol, exponent)
            }
            None => return Err("a unit is missing at the end".to_string()),
        };
        if self.peek() == Some(b'{') {
            return self.annotation(unit);
        }
        Ok(unit)
    }

    /// `{text}`: printable ASCII other than braces.
    fn annotation(&mut self, mut unit: Unit) -> Result<Unit, String> {
        let start = self.at + 1;
        let close = self.text[start..]
            .iter()
            .position(|&c| c == b'}')
            .ok_or_else(|| "an annotation has no '}'".to_string())?;
        let body = &self.text[start..start + close];
        if body.is_empty() {
            return Err("an annotation is empty".to_string());
        }
        if !body.iter().all(|c| (b' '..=b'~').contains(c) && *c != b'{') {
            return Err("an annotation holds printable ASCII other than braces".to_string());
        }
        unit.annotations
            .push(String::from_utf8_lossy(body).into_owned());
        self.at = start + close + 1;
        Ok(unit)
    }

    /// `10*n` or `10^n` (the nearest binary64 to 10^n), or an integer.
    fn number(&mut self) -> Result<Unit, String> {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.at += 1;
        }
        let digits = std::str::from_utf8(&self.text[start..self.at]).unwrap_or_default();
        if digits.len() > 1 && digits.starts_with('0') {
            return Err(format!("{digits} has a leading zero"));
        }
        if digits == "10" && matches!(self.peek(), Some(b'*' | b'^')) {
            self.at += 1;
            let exponent = self.exponent()?;
            if self.at == start + 3 {
                return Err("10* needs an exponent".to_string());
            }
            let factor: f64 = format!("1e{exponent}")
                .parse()
                .map_err(|_| "a power of ten".to_string())?;
            return Ok(Unit {
                factor,
                ..Unit::one()
            });
        }
        let factor: f64 = digits
            .parse()
            .map_err(|_| format!("{digits:?} isn't a number"))?;
        Ok(Unit {
            factor,
            ..Unit::one()
        })
    }

    /// `[+|-]digits`, or 1 when there are none.
    fn exponent(&mut self) -> Result<i32, String> {
        let start = self.at;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.at += 1;
        }
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.at += 1;
        }
        let text = std::str::from_utf8(&self.text[start..self.at]).unwrap_or_default();
        match text {
            "" => Ok(1),
            "+" | "-" => Err(format!("a sign with no exponent at byte {start}")),
            _ => match text.parse::<i32>() {
                Ok(0) => Err(format!("a zero exponent at byte {start}")),
                Ok(n) if n.unsigned_abs() <= 99 => Ok(n),
                _ => Err(format!("exponent {text} is past ±99")),
            },
        }
    }

    /// A bracketed atom (`[ft_i]`), or a run of other characters: an atom,
    /// or a prefix and a metric atom.
    fn symbol(&mut self) -> Result<Unit, String> {
        let start = self.at;
        if self.peek() == Some(b'[') {
            let close = self.text[start..]
                .iter()
                .position(|&c| c == b']')
                .ok_or_else(|| "a '[' has no ']'".to_string())?;
            self.at = start + close + 1;
        } else {
            while self.peek().is_some_and(|c| {
                !matches!(c, b'.' | b'/' | b'(' | b')' | b'{' | b'[' | b'+' | b'-')
                    && !c.is_ascii_digit()
            }) {
                self.at += 1;
            }
        }
        let code = std::str::from_utf8(&self.text[start..self.at])
            .map_err(|_| "a unit is ASCII".to_string())?;
        if code.is_empty() {
            return Err(format!("a unit is missing at byte {start}"));
        }
        if let Some(atom) = ATOMS.iter().find(|atom| atom.code == code) {
            return Ok(from_atom(atom, 1.0));
        }
        for (prefix, scale) in PREFIXES {
            if let Some(rest) = code.strip_prefix(prefix)
                && let Some(atom) = ATOMS.iter().find(|atom| atom.metric && atom.code == rest)
            {
                return Ok(from_atom(atom, *scale));
            }
        }
        if code == "Cel" || code == "[degF]" {
            return Err(format!(
                "{code} stands alone; it can't be part of a product"
            ));
        }
        Err(format!(
            "{code:?} is no unit or prefixed metric unit MDL knows"
        ))
    }
}

fn from_atom(atom: &Atom, prefix: f64) -> Unit {
    Unit {
        dimension: atom.dimension,
        factor: prefix * atom.factor,
        scale: None,
        annotations: Vec::new(),
    }
}

#[cfg(test)]
mod tests;
