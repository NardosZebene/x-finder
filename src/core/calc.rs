//! Calculator for the command palette: `2+3*4`, `(1+2)^3`, `sqrt(2)`,
//! `15% of 80`, and file sizes: `1.5gb in mb`, `700mb*3`.
//!
//! Sizes use 1 KB = 1000 bytes, the same as the file list (and macOS).

const UNITS: [(&str, f64); 5] = [("tb", 1e12), ("gb", 1e9), ("mb", 1e6), ("kb", 1e3), ("b", 1.0)];

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    /// A size unit appeared (the answer is a size).
    sized: bool,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn word(&mut self) -> Option<String> {
        self.ws();
        let start = self.i;
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphabetic()) {
            self.i += 1;
        }
        (self.i > start).then(|| String::from_utf8_lossy(&self.s[start..self.i]).to_lowercase())
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        loop {
            if self.eat(b'+') {
                v += self.term()?;
            } else if self.eat(b'-') {
                v -= self.term()?;
            } else {
                return Some(v);
            }
        }
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.power()?;
        loop {
            if self.eat(b'*') || self.eat(b'x') {
                v *= self.power()?;
            } else if self.eat(b'/') {
                v /= self.power()?;
            } else {
                return Some(v);
            }
        }
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.unary()?;
        if self.eat(b'^') {
            // Right-associative: 2^3^2 = 2^9.
            return Some(base.powf(self.power()?));
        }
        Some(base)
    }

    fn unary(&mut self) -> Option<f64> {
        if self.eat(b'-') {
            return Some(-self.unary()?);
        }
        if self.eat(b'+') {
            return self.unary();
        }
        self.postfix()
    }

    /// A value, then `%` (percent; "15% of 80" = 12).
    fn postfix(&mut self) -> Option<f64> {
        let v = self.primary()?;
        if self.eat(b'%') {
            let save = self.i;
            if self.word().as_deref() == Some("of") {
                return Some(v / 100.0 * self.unary()?);
            }
            self.i = save;
            return Some(v / 100.0);
        }
        Some(v)
    }

    fn primary(&mut self) -> Option<f64> {
        self.ws();
        if self.eat(b'(') {
            let v = self.expr()?;
            return self.eat(b')').then_some(v);
        }
        let start = self.i;
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == b'.') {
            self.i += 1;
        }
        if self.i > start {
            let n: f64 = std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()?;
            // Optional size unit right after the number ("1.5gb", "200 mb").
            let save = self.i;
            if let Some(w) = self.word() {
                if let Some((_, m)) = UNITS.iter().find(|(u, _)| *u == w) {
                    self.sized = true;
                    return Some(n * m);
                }
                self.i = save;
            }
            return Some(n);
        }
        let save = self.i;
        let w = self.word()?;
        match w.as_str() {
            "pi" => Some(std::f64::consts::PI),
            "e" => Some(std::f64::consts::E),
            f @ ("sqrt" | "abs" | "round" | "floor" | "ceil" | "ln" | "log" | "sin" | "cos" | "tan") => {
                if !self.eat(b'(') {
                    self.i = save;
                    return None;
                }
                let x = self.expr()?;
                if !self.eat(b')') {
                    return None;
                }
                Some(match f {
                    "sqrt" => x.sqrt(),
                    "abs" => x.abs(),
                    "round" => x.round(),
                    "floor" => x.floor(),
                    "ceil" => x.ceil(),
                    "ln" => x.ln(),
                    "log" => x.log10(),
                    "sin" => x.sin(),
                    "cos" => x.cos(),
                    _ => x.tan(),
                })
            }
            _ => {
                self.i = save;
                None
            }
        }
    }
}

/// Plain number, no float noise: 0.1+0.2 → "0.3", 1e21 → "1e21".
fn number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        let digits = super::format::thousands(v.abs() as usize);
        return if v < 0.0 { format!("-{digits}") } else { digits.to_string() };
    }
    if v.abs() >= 1e15 || (v != 0.0 && v.abs() < 1e-6) {
        return format!("{v:e}");
    }
    let s = format!("{v:.10}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Evaluates `input` when it looks like a calculation; `None` for anything
/// else (so folder names such as "2024" or "1-2 notes" aren't answered).
/// Returns the answer as text, e.g. "14" or "1,536 MB".
pub fn eval(input: &str) -> Option<String> {
    let t = input.trim().trim_start_matches('=').trim();
    if t.is_empty() || !t.bytes().any(|c| c.is_ascii_digit()) {
        return None;
    }
    // "<size> in <unit>": convert.
    let lower = t.to_lowercase();
    let (expr, target) = match lower.rsplit_once(" in ") {
        Some((e, u)) => match UNITS.iter().find(|(name, _)| *name == u.trim()) {
            Some(&(name, m)) => (e.to_string(), Some((name, m))),
            None => (lower.clone(), None),
        },
        None => (lower.clone(), None),
    };
    let mut p = Parser { s: expr.as_bytes(), i: 0, sized: false };
    let v = p.expr()?;
    p.ws();
    if p.i != p.s.len() || !v.is_finite() {
        return None;
    }
    // Something must have been calculated: an operator, a function or a
    // conversion, not just "42" or "2024".
    let did_math = target.is_some()
        || expr.bytes().skip(1).any(|c| b"+-*/^%x(".contains(&c))
        || expr.starts_with('(')
        || expr.starts_with('-') && expr.len() > 1
        || ["sqrt", "abs", "round", "floor", "ceil", "ln", "log", "sin", "cos", "tan", "pi"].iter().any(|f| expr.contains(f));
    if !did_math {
        return None;
    }
    Some(match target {
        Some((name, m)) => format!("{} {}", number(v / m), name.to_uppercase()),
        None if p.sized => format!("{} ({} bytes)", super::format::size(v.max(0.0) as u64), number(v.round())),
        None => number(v),
    })
}

#[cfg(test)]
mod tests {
    use super::eval;

    #[test]
    fn arithmetic() {
        assert_eq!(eval("2+3").as_deref(), Some("5"));
        assert_eq!(eval("2+3*4").as_deref(), Some("14"));
        assert_eq!(eval("(2+3)*4").as_deref(), Some("20"));
        assert_eq!(eval("10/4").as_deref(), Some("2.5"));
        assert_eq!(eval("2^10").as_deref(), Some("1,024"));
        assert_eq!(eval("0.1+0.2").as_deref(), Some("0.3"));
        assert_eq!(eval("-5+2").as_deref(), Some("-3"));
        assert_eq!(eval("= 7 * 6").as_deref(), Some("42"));
        assert_eq!(eval("sqrt(16)").as_deref(), Some("4"));
        assert_eq!(eval("15% of 80").as_deref(), Some("12"));
        assert_eq!(eval("1000000*3").as_deref(), Some("3,000,000"));
    }

    #[test]
    fn sizes() {
        assert_eq!(eval("1.5gb in mb").as_deref(), Some("1,500 MB"));
        assert_eq!(eval("2500 kb in mb").as_deref(), Some("2.5 MB"));
        assert!(eval("700mb*3").unwrap().contains("bytes"));
    }

    #[test]
    fn not_math() {
        assert_eq!(eval("2024"), None);
        assert_eq!(eval("downloads"), None);
        assert_eq!(eval("1-2 notes"), None);
        assert_eq!(eval("mkdir a, b"), None);
        assert_eq!(eval("2+"), None);
        assert_eq!(eval("1/0"), None);
    }
}
