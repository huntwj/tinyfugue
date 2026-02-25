//! Built-in TF functions.
//!
//! Each function receives a `Vec<Value>` of already-evaluated arguments and
//! returns `Result<Value, String>`.  The dispatcher is called from the
//! interpreter's `call_fn` implementation.

use super::value::Value;

/// Dispatch a built-in function call.
///
/// Returns `None` if the function name is not a built-in (caller should then
/// try user-defined macros or return an error).
pub fn call_builtin(name: &str, args: Vec<Value>) -> Option<Result<Value, String>> {
    // Inner function returns Result<Option<Value>, String>:
    //   Ok(None)    → not a builtin
    //   Ok(Some(v)) → success
    //   Err(e)      → builtin call failed
    // `.transpose()` converts that to Option<Result<Value, String>>.
    fn inner(name: &str, args: Vec<Value>) -> Result<Option<Value>, String> {
        Ok(Some(match name {
            // ── String functions ─────────────────────────────────────────────
            "strlen" => {
                let s = get_str(&args, 0, name)?;
                Value::Int(s.chars().count() as i64)
            }
            "strcat" => {
                let mut out = String::new();
                for a in &args {
                    out.push_str(&a.as_str());
                }
                Value::Str(out)
            }
            "substr" => {
                let s = get_str(&args, 0, name)?;
                let pos = get_int(&args, 1, name)? as usize;
                let len_arg = args.get(2).map(|v| v.as_int() as usize);
                let chars: Vec<char> = s.chars().collect();
                let start = pos.min(chars.len());
                let slice = match len_arg {
                    Some(n) => &chars[start..((start + n).min(chars.len()))],
                    None => &chars[start..],
                };
                Value::Str(slice.iter().collect())
            }
            "strcmp" => {
                let a = get_str(&args, 0, name)?;
                let b = get_str(&args, 1, name)?;
                Value::Int(match a.cmp(&b) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                })
            }
            "strncmp" => {
                let a = get_str(&args, 0, name)?;
                let b = get_str(&args, 1, name)?;
                let n = get_int(&args, 2, name)? as usize;
                let ac: String = a.chars().take(n).collect();
                let bc: String = b.chars().take(n).collect();
                Value::Int(match ac.cmp(&bc) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                })
            }
            "toupper" => {
                let s = get_str(&args, 0, name)?;
                Value::Str(s.to_uppercase())
            }
            "tolower" => {
                let s = get_str(&args, 0, name)?;
                Value::Str(s.to_lowercase())
            }
            "strstr" => {
                let haystack = get_str(&args, 0, name)?;
                let needle = get_str(&args, 1, name)?;
                Value::Int(match haystack.find(&needle) {
                    Some(i) => i as i64,
                    None => -1,
                })
            }
            "strrep" => {
                // strrep(str, count) — repeat str n times
                let s = get_str(&args, 0, name)?;
                let n = get_int(&args, 1, name)?.max(0) as usize;
                Value::Str(s.repeat(n))
            }
            "replace" => {
                // replace(haystack, needle, replacement)
                let haystack = get_str(&args, 0, name)?;
                let needle = get_str(&args, 1, name)?;
                let repl = get_str(&args, 2, name)?;
                Value::Str(haystack.replace(&needle, &repl))
            }
            "pad" => {
                // pad(str, width[, char]) — right-pad with spaces (or given char)
                let s = get_str(&args, 0, name)?;
                let width = get_int(&args, 1, name)? as usize;
                let pad_c = args
                    .get(2)
                    .map(|v| v.as_str().chars().next().unwrap_or(' '))
                    .unwrap_or(' ');
                let cur = s.chars().count();
                let padded = if cur < width {
                    let padding: String = std::iter::repeat_n(pad_c, width - cur).collect();
                    s + &padding
                } else {
                    s
                };
                Value::Str(padded)
            }

            // ── Math functions ───────────────────────────────────────────────
            "abs" => {
                let v = args
                    .into_iter()
                    .next()
                    .ok_or_else(|| format!("{name}: too few args"))?;
                match v {
                    Value::Int(n) => Value::Int(n.abs()),
                    Value::Float(x) => Value::Float(x.abs()),
                    Value::Str(s) => {
                        if let Ok(n) = s.trim().parse::<i64>() {
                            Value::Int(n.abs())
                        } else if let Ok(x) = s.trim().parse::<f64>() {
                            Value::Float(x.abs())
                        } else {
                            Value::Int(0)
                        }
                    }
                }
            }
            "mod" => {
                let a = get_int(&args, 0, name)?;
                let b = get_int(&args, 1, name)?;
                if b == 0 {
                    return Err("mod: modulo by zero".into());
                }
                Value::Int(a % b)
            }
            "rand" => {
                // Deterministic-ish: use a simple LCG with time-based seed.
                let max = args.first().map(|v| v.as_int().max(1)).unwrap_or(100);
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(12345) as i64;
                Value::Int(seed.abs() % max)
            }
            "trunc" => {
                let x = get_float(&args, 0, name)?;
                Value::Int(x.trunc() as i64)
            }
            "sqrt" => Value::Float(get_float(&args, 0, name)?.sqrt()),
            "sin" => Value::Float(get_float(&args, 0, name)?.sin()),
            "cos" => Value::Float(get_float(&args, 0, name)?.cos()),
            "tan" => Value::Float(get_float(&args, 0, name)?.tan()),
            "exp" => Value::Float(get_float(&args, 0, name)?.exp()),
            "ln" => Value::Float(get_float(&args, 0, name)?.ln()),
            "log10" => Value::Float(get_float(&args, 0, name)?.log10()),
            "pow" => {
                let base = get_float(&args, 0, name)?;
                let exp = get_float(&args, 1, name)?;
                Value::Float(base.powf(exp))
            }
            "asin" => Value::Float(get_float(&args, 0, name)?.asin()),
            "acos" => Value::Float(get_float(&args, 0, name)?.acos()),
            "atan" => {
                let y = get_float(&args, 0, name)?;
                if args.len() >= 2 {
                    let x = get_float(&args, 1, name)?;
                    Value::Float(y.atan2(x))
                } else {
                    Value::Float(y.atan())
                }
            }

            // ── Type inspection ──────────────────────────────────────────────
            "whatis" => {
                let v = args
                    .into_iter()
                    .next()
                    .ok_or_else(|| format!("{name}: too few args"))?;
                Value::Str(v.type_name().to_owned())
            }
            // TF reports the OS family, not the kernel name: "unix" on all
            // POSIX-like systems (Linux, macOS, BSDs), "os/2" on OS/2.
            "systype" => Value::Str(
                if cfg!(target_os = "windows") { "windows" }
                else { "unix" }
                .to_owned()
            ),
            "getpid" => {
                Value::Int(std::process::id() as i64)
            }

            // ── Time functions ───────────────────────────────────────────────
            "time" => {
                // time() → seconds since Unix epoch
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                Value::Int(secs as i64)
            }
            "ftime" => {
                // ftime(format[, time]) — format a Unix timestamp.
                // Uses strftime-style format codes (%H, %M, %Y, etc.).
                let fmt = get_str(&args, 0, name)?;
                let secs = args.get(1).map(|v| v.as_int() as u64).unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                });
                Value::Str(ftime_format(&fmt, secs))
            }
            "mktime" => {
                // mktime(time_str) — parse time string, return Unix timestamp (UTC).
                // Accepted formats (matching common ftime output):
                //   "YYYY-MM-DD HH:MM:SS"  "YYYY/MM/DD HH:MM:SS"
                //   "HH:MM:SS"             bare integer (pass-through)
                let s = get_str(&args, 0, name)?;
                let ts = mktime_parse(s.trim()).unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0)
                });
                Value::Int(ts)
            }

            // ── Attribute / display ──────────────────────────────────────────
            "decode_attr" => {
                // decode_attr(text[, attr[, pageable]]) → text (passthrough)
                Value::Str(get_str(&args, 0, name).unwrap_or_default())
            }
            "encode_attr" => {
                Value::Str(get_str(&args, 0, name).unwrap_or_default())
            }
            "attrout" => Value::Int(0),

            // ── Character functions ───────────────────────────────────────────
            "ascii" => {
                // ascii(str) → ordinal of first character
                let s = get_str(&args, 0, name)?;
                let ch = s.chars().next().unwrap_or('\0');
                Value::Int(ch as i64)
            }
            "char" => {
                // char(n) → string containing that Unicode code point
                let n = get_int(&args, 0, name)?;
                let ch = char::from_u32(n as u32).unwrap_or('\u{FFFD}');
                Value::Str(ch.to_string())
            }

            _ => return Ok(None),
        }))
    }

    inner(name, args).transpose()
}

// ── mktime helper ────────────────────────────────────────────────────────────

/// Parse a time string and return a UTC Unix timestamp, or `None` on failure.
///
/// Accepted formats:
/// - `"YYYY-MM-DD HH:MM:SS"` or `"YYYY/MM/DD HH:MM:SS"`
/// - `"HH:MM:SS"` (uses today's UTC date)
/// - A bare integer string (returned as-is)
fn mktime_parse(s: &str) -> Option<i64> {
    // Bare integer pass-through.
    if let Ok(n) = s.parse::<i64>() {
        return Some(n);
    }

    // Determine current UTC date for the time-only form.
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let today_days = now_secs.div_euclid(86400);
    let (today_y, today_m, today_d) = civil_from_days(today_days);

    // Try "YYYY-MM-DD HH:MM:SS" or "YYYY/MM/DD HH:MM:SS".
    let parts: Vec<&str> = s.splitn(2, char::is_whitespace).collect();
    if parts.len() == 2 {
        let date_part = parts[0];
        let time_part = parts[1];
        let sep = if date_part.contains('-') { '-' } else { '/' };
        let dp: Vec<&str> = date_part.splitn(3, sep).collect();
        let tp: Vec<&str> = time_part.splitn(3, ':').collect();
        if dp.len() == 3 && tp.len() == 3 {
            let y: i64 = dp[0].parse().ok()?;
            let mo: u32 = dp[1].parse().ok()?;
            let d: u32  = dp[2].parse().ok()?;
            let h: i64  = tp[0].parse().ok()?;
            let mi: i64 = tp[1].parse().ok()?;
            let sc: i64 = tp[2].parse().ok()?;
            let days = days_from_civil(y, mo, d);
            return Some(days * 86400 + h * 3600 + mi * 60 + sc);
        }
    }

    // Try "HH:MM:SS".
    let tp: Vec<&str> = s.splitn(3, ':').collect();
    if tp.len() == 3 {
        let h: i64  = tp[0].parse().ok()?;
        let mi: i64 = tp[1].parse().ok()?;
        let sc: i64 = tp[2].parse().ok()?;
        let days = days_from_civil(today_y, today_m, today_d);
        return Some(days * 86400 + h * 3600 + mi * 60 + sc);
    }

    None
}

/// Convert (year, month 1-12, day 1-31) to days since Unix epoch.
/// Algorithm: Howard Hinnant's `days_from_civil`.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// Decompose days-since-epoch into (year, month 1-12, day 1-31).
/// Mirrors the algorithm used in `ftime_format`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe as i64 + era * 400 + if mo <= 2 { 1 } else { 0 };
    (y, mo, d)
}

// ── ftime helper ─────────────────────────────────────────────────────────────

/// Format a Unix timestamp using strftime-style codes (UTC).
///
/// Supported codes: `%H %M %S %Y %y %m %d %j %A %a %B %b %e %n %t %%`.
fn ftime_format(fmt: &str, secs: u64) -> String {
    let secs = secs as i64;
    let day_secs = secs.rem_euclid(86400) as u32;
    let days = secs.div_euclid(86400);

    let h = day_secs / 3600;
    let m = (day_secs % 3600) / 60;
    let s = day_secs % 60;

    // Decompose days-since-epoch to (year, month [1-12], day [1-31]).
    // Algorithm: Howard Hinnant's civil_from_days.
    let (year, month, day) = {
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u32;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let mo = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe as i64 + era * 400 + if mo <= 2 { 1 } else { 0 };
        (y, mo, d)
    };

    // Day of year (1-366).
    let yday: u32 = {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let mdays: [u32; 12] = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        mdays[..month as usize - 1].iter().sum::<u32>() + day
    };

    // Day of week (0=Sunday).
    let wday = ((days + 4).rem_euclid(7)) as u32; // epoch was Thursday=4

    let month_names = ["January","February","March","April","May","June",
                       "July","August","September","October","November","December"];
    let day_names   = ["Sunday","Monday","Tuesday","Wednesday","Thursday","Friday","Saturday"];

    let mut out = String::with_capacity(fmt.len() + 16);
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            None        => { out.push('%'); }
            Some('%')   => out.push('%'),
            Some('n')   => out.push('\n'),
            Some('t')   => out.push('\t'),
            Some('H')   => out.push_str(&format!("{h:02}")),
            Some('M')   => out.push_str(&format!("{m:02}")),
            Some('S')   => out.push_str(&format!("{s:02}")),
            Some('Y')   => out.push_str(&format!("{year}")),
            Some('y')   => out.push_str(&format!("{:02}", year.rem_euclid(100))),
            Some('m')   => out.push_str(&format!("{month:02}")),
            Some('d')   => out.push_str(&format!("{day:02}")),
            Some('e')   => out.push_str(&format!("{day:2}")),
            Some('j')   => out.push_str(&format!("{yday:03}")),
            Some('A')   => out.push_str(day_names[wday as usize]),
            Some('a')   => out.push_str(&day_names[wday as usize][..3]),
            Some('B')   => out.push_str(month_names[month as usize - 1]),
            Some('b') | Some('h') => out.push_str(&month_names[month as usize - 1][..3]),
            Some('p')   => out.push_str(if h < 12 { "AM" } else { "PM" }),
            Some('I')   => out.push_str(&format!("{:02}", if h.is_multiple_of(12) { 12 } else { h % 12 })),
            Some('w')   => out.push_str(&format!("{wday}")),
            Some(other) => { out.push('%'); out.push(other); }
        }
    }
    out
}

// ── Argument accessors ────────────────────────────────────────────────────────

fn get_str(args: &[Value], idx: usize, name: &str) -> Result<String, String> {
    args.get(idx)
        .map(|v| v.as_str())
        .ok_or_else(|| format!("{name}: argument {idx} missing"))
}

fn get_int(args: &[Value], idx: usize, name: &str) -> Result<i64, String> {
    args.get(idx)
        .map(|v| v.as_int())
        .ok_or_else(|| format!("{name}: argument {idx} missing"))
}

fn get_float(args: &[Value], idx: usize, name: &str) -> Result<f64, String> {
    args.get(idx)
        .map(|v| v.as_float())
        .ok_or_else(|| format!("{name}: argument {idx} missing"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: Vec<Value>) -> Value {
        call_builtin(name, args)
            .expect("not a builtin")
            .expect("call failed")
    }

    #[test]
    fn strlen() {
        assert_eq!(
            call("strlen", vec![Value::Str("hello".into())]),
            Value::Int(5)
        );
    }

    #[test]
    fn strcat() {
        let v = call(
            "strcat",
            vec![Value::Str("foo".into()), Value::Str("bar".into())],
        );
        assert_eq!(v, Value::Str("foobar".into()));
    }

    #[test]
    fn substr_from() {
        let v = call("substr", vec![Value::Str("hello".into()), Value::Int(2)]);
        assert_eq!(v, Value::Str("llo".into()));
    }

    #[test]
    fn substr_with_len() {
        let v = call(
            "substr",
            vec![Value::Str("hello".into()), Value::Int(1), Value::Int(3)],
        );
        assert_eq!(v, Value::Str("ell".into()));
    }

    #[test]
    fn strcmp_lt() {
        assert_eq!(call("strcmp", vec!["a".into(), "b".into()]), Value::Int(-1));
    }

    #[test]
    fn strcmp_eq() {
        assert_eq!(call("strcmp", vec!["x".into(), "x".into()]), Value::Int(0));
    }

    #[test]
    fn toupper_lower() {
        assert_eq!(
            call("toupper", vec!["Hello".into()]),
            Value::Str("HELLO".into())
        );
        assert_eq!(
            call("tolower", vec!["Hello".into()]),
            Value::Str("hello".into())
        );
    }

    #[test]
    fn strstr_found() {
        assert_eq!(
            call("strstr", vec!["foobar".into(), "bar".into()]),
            Value::Int(3)
        );
    }

    #[test]
    fn strstr_not_found() {
        assert_eq!(
            call("strstr", vec!["foobar".into(), "xyz".into()]),
            Value::Int(-1)
        );
    }

    #[test]
    fn strrep() {
        assert_eq!(
            call("strrep", vec!["ab".into(), Value::Int(3)]),
            Value::Str("ababab".into())
        );
    }

    #[test]
    fn replace_fn() {
        let v = call(
            "replace",
            vec!["hello world".into(), "world".into(), "Rust".into()],
        );
        assert_eq!(v, Value::Str("hello Rust".into()));
    }

    #[test]
    fn pad_fn() {
        let v = call("pad", vec!["hi".into(), Value::Int(5)]);
        assert_eq!(v, Value::Str("hi   ".into()));
    }

    #[test]
    fn abs_int() {
        assert_eq!(call("abs", vec![Value::Int(-7)]), Value::Int(7));
    }

    #[test]
    fn abs_float() {
        assert_eq!(call("abs", vec![Value::Float(-1.5)]), Value::Float(1.5));
    }

    #[test]
    fn trunc_fn() {
        assert_eq!(call("trunc", vec![Value::Float(3.9)]), Value::Int(3));
    }

    #[test]
    fn sqrt_fn() {
        assert_eq!(call("sqrt", vec![Value::Float(4.0)]), Value::Float(2.0));
    }

    #[test]
    fn pow_fn() {
        assert_eq!(
            call("pow", vec![Value::Float(2.0), Value::Float(10.0)]),
            Value::Float(1024.0)
        );
    }

    #[test]
    fn whatis() {
        assert_eq!(
            call("whatis", vec![Value::Int(1)]),
            Value::Str("integer".into())
        );
        assert_eq!(
            call("whatis", vec![Value::Float(1.0)]),
            Value::Str("real".into())
        );
        assert_eq!(
            call("whatis", vec!["hi".into()]),
            Value::Str("string".into())
        );
    }

    #[test]
    fn ascii_char() {
        assert_eq!(call("ascii", vec!["A".into()]), Value::Int(65));
        assert_eq!(call("char", vec![Value::Int(65)]), Value::Str("A".into()));
    }

    #[test]
    fn unknown_builtin_returns_none() {
        assert!(call_builtin("no_such_fn", vec![]).is_none());
    }

    #[test]
    fn pad_multibyte_char() {
        let v = call(
            "pad",
            vec![
                Value::Str("hi".into()),
                Value::Int(5),
                Value::Str("€".into()),
            ],
        );
        assert_eq!(v, Value::Str("hi€€€".into()));
    }

    #[test]
    fn mktime_bare_int() {
        assert_eq!(
            call("mktime", vec![Value::Str("1000000".into())]),
            Value::Int(1_000_000)
        );
    }

    #[test]
    fn mktime_full_datetime() {
        // 1970-01-01 00:00:00 UTC == 0
        assert_eq!(
            call("mktime", vec![Value::Str("1970-01-01 00:00:00".into())]),
            Value::Int(0)
        );
        // 1970-01-01 00:01:00 UTC == 60
        assert_eq!(
            call("mktime", vec![Value::Str("1970-01-01 00:01:00".into())]),
            Value::Int(60)
        );
    }

    #[test]
    fn mktime_slash_separator() {
        assert_eq!(
            call("mktime", vec![Value::Str("1970/01/01 00:00:00".into())]),
            Value::Int(0)
        );
    }

    #[test]
    fn mktime_roundtrip_with_ftime() {
        // ftime then mktime should return the original timestamp (UTC, whole seconds).
        let ts = 1_700_000_000i64;
        let formatted = match call("ftime", vec![
            Value::Str("%Y-%m-%d %H:%M:%S".into()),
            Value::Int(ts),
        ]) {
            Value::Str(s) => s,
            other => panic!("expected Str, got {other:?}"),
        };
        assert_eq!(
            call("mktime", vec![Value::Str(formatted)]),
            Value::Int(ts)
        );
    }

    #[test]
    fn substr_out_of_bounds_and_len() {
        let v = call("substr", vec![Value::Str("hello".into()), Value::Int(10)]);
        assert_eq!(v, Value::Str("".into()));

        let v2 = call(
            "substr",
            vec![Value::Str("hello".into()), Value::Int(3), Value::Int(10)],
        );
        assert_eq!(v2, Value::Str("lo".into()));
    }
}
