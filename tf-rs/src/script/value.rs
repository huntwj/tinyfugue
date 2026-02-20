//! Runtime value type for the TF scripting language.
//!
//! TF is dynamically typed; every value is a string at heart, but the
//! interpreter coerces freely to integers and floats when needed.

use std::fmt;

/// A TF script runtime value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
}

impl Default for Value {
    fn default() -> Self {
        Value::Str(String::new())
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(x) => {
                // TF prints floats without trailing zeros where possible.
                if x.fract() == 0.0 && x.abs() < 1e15 {
                    write!(f, "{:.1}", x)
                } else {
                    write!(f, "{x}")
                }
            }
            Value::Str(s) => write!(f, "{s}"),
        }
    }
}

impl Value {
    /// Coerce to boolean: `0`, `""`, and `"0"` are falsy.
    pub fn as_bool(&self) -> bool {
        match self {
            Value::Int(n) => *n != 0,
            Value::Float(x) => *x != 0.0,
            Value::Str(s) => !s.is_empty() && s != "0",
        }
    }

    /// Coerce to `i64` (returns 0 on failure for Str).
    pub fn as_int(&self) -> i64 {
        match self {
            Value::Int(n) => *n,
            Value::Float(x) => *x as i64,
            Value::Str(s) => s.trim().parse().unwrap_or(0),
        }
    }

    /// Coerce to `f64`.
    pub fn as_float(&self) -> f64 {
        match self {
            Value::Int(n) => *n as f64,
            Value::Float(x) => *x,
            Value::Str(s) => s.trim().parse().unwrap_or(0.0),
        }
    }

    /// Coerce to a string (clones for Str, formats for numeric variants).
    pub fn as_str(&self) -> String {
        self.to_string()
    }

    /// Name of the type, as returned by `whatis()`.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "integer",
            Value::Float(_) => "real",
            Value::Str(_) => "string",
        }
    }

    // ── Arithmetic helpers ────────────────────────────────────────────────────

    /// Determine the common numeric type for a binary operation.
    fn numeric_promote(a: &Value, b: &Value) -> (f64, f64, bool) {
        // Returns (a_f64, b_f64, is_float)
        let is_float = matches!(a, Value::Float(_)) || matches!(b, Value::Float(_))
            || matches!(a, Value::Str(_)) && a.as_str().contains('.')
            || matches!(b, Value::Str(_)) && b.as_str().contains('.');
        (a.as_float(), b.as_float(), is_float)
    }

    fn make_numeric(f: f64, is_float: bool) -> Value {
        if is_float {
            Value::Float(f)
        } else {
            Value::Int(f as i64)
        }
    }

    pub fn arith_add(&self, rhs: &Value) -> Value {
        // String concatenation if either operand is a non-numeric string.
        if let (Value::Str(a), _) | (_, Value::Str(a)) = (self, rhs) {
            let _ = a; // borrow check trick — check both
        }
        // Try numeric first
        let (a, b, is_float) = Self::numeric_promote(self, rhs);
        Self::make_numeric(a + b, is_float)
    }

    pub fn arith_sub(&self, rhs: &Value) -> Value {
        let (a, b, is_float) = Self::numeric_promote(self, rhs);
        Self::make_numeric(a - b, is_float)
    }

    pub fn arith_mul(&self, rhs: &Value) -> Value {
        let (a, b, is_float) = Self::numeric_promote(self, rhs);
        Self::make_numeric(a * b, is_float)
    }

    pub fn arith_div(&self, rhs: &Value) -> Result<Value, String> {
        let (a, b, is_float) = Self::numeric_promote(self, rhs);
        if b == 0.0 {
            return Err("division by zero".into());
        }
        Ok(Self::make_numeric(a / b, is_float))
    }

    pub fn arith_rem(&self, rhs: &Value) -> Result<Value, String> {
        let (a, b, is_float) = Self::numeric_promote(self, rhs);
        if b == 0.0 {
            return Err("modulo by zero".into());
        }
        Ok(Self::make_numeric(a % b, is_float))
    }

    pub fn arith_neg(&self) -> Value {
        match self {
            Value::Int(n) => Value::Int(-n),
            Value::Float(x) => Value::Float(-x),
            Value::Str(s) => {
                if let Ok(n) = s.trim().parse::<i64>() {
                    Value::Int(-n)
                } else if let Ok(x) = s.trim().parse::<f64>() {
                    Value::Float(-x)
                } else {
                    Value::Int(0)
                }
            }
        }
    }

    /// Relational comparison: returns -1, 0, or 1.
    pub fn cmp_value(&self, rhs: &Value) -> std::cmp::Ordering {
        // Numeric comparison when both sides parse as numbers.
        match (self, rhs) {
            (Value::Str(a), Value::Str(b)) => {
                // Try numeric first
                let an = a.trim().parse::<f64>();
                let bn = b.trim().parse::<f64>();
                match (an, bn) {
                    (Ok(af), Ok(bf)) => af.partial_cmp(&bf).unwrap_or(std::cmp::Ordering::Equal),
                    _ => a.cmp(b),
                }
            }
            _ => {
                let (a, b, _) = Self::numeric_promote(self, rhs);
                a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
            }
        }
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Int(n)
    }
}

impl From<f64> for Value {
    fn from(x: f64) -> Self {
        Value::Float(x)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_owned())
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Int(if b { 1 } else { 0 })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_int() {
        assert_eq!(Value::Int(42).to_string(), "42");
        assert_eq!(Value::Int(-7).to_string(), "-7");
    }

    #[test]
    fn display_float() {
        assert_eq!(Value::Float(3.14).to_string(), "3.14");
        assert_eq!(Value::Float(1.0).to_string(), "1.0");
    }

    #[test]
    fn display_str() {
        assert_eq!(Value::Str("hello".into()).to_string(), "hello");
    }

    #[test]
    fn as_bool() {
        assert!(Value::Int(1).as_bool());
        assert!(!Value::Int(0).as_bool());
        assert!(Value::Str("hello".into()).as_bool());
        assert!(!Value::Str("".into()).as_bool());
        assert!(!Value::Str("0".into()).as_bool());
        assert!(Value::Str("1".into()).as_bool());
    }

    #[test]
    fn as_int_coercions() {
        assert_eq!(Value::Int(5).as_int(), 5);
        assert_eq!(Value::Float(3.9).as_int(), 3);
        assert_eq!(Value::Str("42".into()).as_int(), 42);
        assert_eq!(Value::Str("abc".into()).as_int(), 0);
    }

    #[test]
    fn arithmetic() {
        let a = Value::Int(10);
        let b = Value::Int(3);
        assert_eq!(a.arith_add(&b), Value::Int(13));
        assert_eq!(a.arith_sub(&b), Value::Int(7));
        assert_eq!(a.arith_mul(&b), Value::Int(30));
        assert_eq!(a.arith_div(&b), Ok(Value::Int(3)));
        assert_eq!(a.arith_rem(&b), Ok(Value::Int(1)));
    }

    #[test]
    fn div_by_zero() {
        assert!(Value::Int(1).arith_div(&Value::Int(0)).is_err());
        assert!(Value::Int(1).arith_rem(&Value::Int(0)).is_err());
    }

    #[test]
    fn float_promotion() {
        let a = Value::Int(7);
        let b = Value::Float(2.0);
        assert_eq!(a.arith_add(&b), Value::Float(9.0));
    }

    #[test]
    fn neg() {
        assert_eq!(Value::Int(5).arith_neg(), Value::Int(-5));
        assert_eq!(Value::Float(1.5).arith_neg(), Value::Float(-1.5));
    }

    #[test]
    fn type_name() {
        assert_eq!(Value::Int(0).type_name(), "integer");
        assert_eq!(Value::Float(0.0).type_name(), "real");
        assert_eq!(Value::Str("".into()).type_name(), "string");
    }

    #[test]
    fn from_impls() {
        let v: Value = 42i64.into();
        assert_eq!(v, Value::Int(42));
        let v: Value = "hi".into();
        assert_eq!(v, Value::Str("hi".into()));
        let v: Value = true.into();
        assert_eq!(v, Value::Int(1));
    }
}
