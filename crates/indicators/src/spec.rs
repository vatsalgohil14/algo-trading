//! Indicator spec strings: `name`, `name(p1, p2, ...)`, optionally followed by
//! `.output`. Examples: `vwap`, `ema(20)`, `orb(15).high`.

use std::fmt;

use crate::IndicatorError;

/// A parsed spec, minus the output selector.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub name: String,
    pub params: Vec<f64>,
}

impl Spec {
    /// Parses `"ema(20)"` or `"orb(15).high"`, returning the spec and the
    /// output name if one was given. Names are case-insensitive.
    pub fn parse(text: &str) -> Result<(Spec, Option<String>), IndicatorError> {
        let err = |why| IndicatorError::Parse {
            spec: text.to_owned(),
            why,
        };
        let s = text.trim();

        let (body, output) = match s.rfind(')') {
            Some(close) => {
                let rest = s.get(close + 1..).unwrap_or("").trim();
                let body = s.get(..=close).unwrap_or(s);
                match rest.strip_prefix('.') {
                    Some(out) => (body, Some(out)),
                    None if rest.is_empty() => (body, None),
                    None => return Err(err("unexpected text after ')'")),
                }
            }
            None => match s.split_once('.') {
                Some((body, out)) => (body, Some(out)),
                None => (s, None),
            },
        };

        let (name, params) = match body.split_once('(') {
            Some((name, rest)) => {
                let inner = rest.strip_suffix(')').ok_or_else(|| err("missing ')'"))?;
                let params = if inner.trim().is_empty() {
                    Vec::new()
                } else {
                    inner
                        .split(',')
                        .map(|p| p.trim().parse::<f64>().ok().filter(|v| v.is_finite()))
                        .collect::<Option<Vec<f64>>>()
                        .ok_or_else(|| err("parameters must be numbers"))?
                };
                (name, params)
            }
            None => (body, Vec::new()),
        };

        let name = name.trim().to_ascii_lowercase();
        let valid_name =
            |n: &str| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid_name(&name) {
            return Err(err("name must be letters, digits or '_'"));
        }
        let output = match output.map(|o| o.trim().to_ascii_lowercase()) {
            Some(o) if !valid_name(&o) => return Err(err("output must be letters, digits or '_'")),
            other => other,
        };
        Ok((Spec { name, params }, output))
    }

    /// Positive integer parameter `index`, at most `max`.
    pub fn period(&self, index: usize, max: usize) -> Result<usize, IndicatorError> {
        let bad = |why: String| IndicatorError::BadParams {
            spec: self.to_string(),
            why,
        };
        let v = *self
            .params
            .get(index)
            .ok_or_else(|| bad(format!("parameter {} is missing", index + 1)))?;
        if v.fract() != 0.0 || v < 1.0 || v > max as f64 {
            return Err(bad(format!(
                "parameter {} must be a whole number from 1 to {max}",
                index + 1
            )));
        }
        Ok(v as usize)
    }

    /// Fails unless there are exactly `n` parameters.
    pub fn expect_params(&self, n: usize) -> Result<(), IndicatorError> {
        if self.params.len() == n {
            Ok(())
        } else {
            Err(IndicatorError::BadParams {
                spec: self.to_string(),
                why: format!("expected {n} parameter(s), got {}", self.params.len()),
            })
        }
    }
}

/// Canonical form, used to recognise the same indicator asked for twice:
/// `EMA( 20 )` and `ema(20)` both print as `ema(20)`.
impl fmt::Display for Spec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        if !self.params.is_empty() {
            f.write_str("(")?;
            for (i, p) in self.params.iter().enumerate() {
                if i > 0 {
                    f.write_str(",")?;
                }
                write!(f, "{p}")?;
            }
            f.write_str(")")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> (String, Option<String>) {
        let (spec, out) = Spec::parse(s).unwrap();
        (spec.to_string(), out)
    }

    #[test]
    fn parses_and_canonicalises() {
        assert_eq!(parse("vwap"), ("vwap".into(), None));
        assert_eq!(parse(" EMA( 20 ) "), ("ema(20)".into(), None));
        assert_eq!(
            parse("orb(15).High"),
            ("orb(15)".into(), Some("high".into()))
        );
        assert_eq!(
            parse("prev_day.close"),
            ("prev_day".into(), Some("close".into()))
        );
        assert_eq!(parse("bb(20, 2.5)"), ("bb(20,2.5)".into(), None));
        assert_eq!(parse("x()"), ("x".into(), None));
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            "",
            "ema(20",
            "ema(abc)",
            "ema(20)x",
            "e-ma",
            "ema(20).",
            "ema(inf)",
            "ema(20).a-b",
        ] {
            assert!(Spec::parse(bad).is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn periods() {
        let (s, _) = Spec::parse("ema(20)").unwrap();
        assert_eq!(s.period(0, 1000).unwrap(), 20);
        assert!(s.period(1, 1000).is_err());
        assert!(s.period(0, 10).is_err());
        for bad in ["ema(0)", "ema(2.5)", "ema(-3)"] {
            assert!(
                Spec::parse(bad).unwrap().0.period(0, 1000).is_err(),
                "{bad}"
            );
        }
        assert!(s.expect_params(1).is_ok());
        assert!(s.expect_params(2).is_err());
    }
}
