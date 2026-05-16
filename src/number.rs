//! Strict number parsing and formatting with Redis rules.

/// Parses `text` as a decimal `i64` the way Redis `string2ll` does.
#[must_use]
pub fn parse_i64(text: &[u8]) -> Option<i64> {
    let (negative, digits) = match text.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, text),
    };
    match digits {
        [b'0'] => return if negative { None } else { Some(0) },
        [] | [b'0', ..] => return None,
        _ => {}
    }
    let mut value: i64 = 0;
    for &byte in digits {
        if !byte.is_ascii_digit() {
            return None;
        }
        let digit = i64::from(byte - b'0');
        value = value.checked_mul(10)?;
        value = if negative {
            value.checked_sub(digit)?
        } else {
            value.checked_add(digit)?
        };
    }
    Some(value)
}

/// Formats a double the way Redis writes it in a reply: the shortest digits that read back.
#[must_use]
pub fn format_double(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.to_owned();
    }
    // 2^62: Redis prints integral doubles up to here as integers
    if value.fract() == 0.0 && value.abs() <= 4_611_686_018_427_387_904.0 {
        return format!("{value:.0}");
    }
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let point: i32 = exponent.parse().unwrap_or(0);
    let count = i32::try_from(digits.len()).unwrap_or(i32::MAX);
    let sign = if value < 0.0 { "-" } else { "" };
    format!(
        "{sign}{}",
        layout(
            &digits,
            point - (count - 1),
            count,
            point.abs(),
            value < 0.0
        )
    )
}

fn layout(digits: &str, k: i32, count: i32, exp: i32, negative: bool) -> String {
    if k >= 0 && exp < count + 7 {
        return format!("{digits}{}", "0".repeat(usize::try_from(k).unwrap_or(0)));
    }
    if k < 0 && (k > -7 || exp < 4) {
        let offset = count - k.abs();
        if offset <= 0 {
            return format!(
                "0.{}{digits}",
                "0".repeat(usize::try_from(-offset).unwrap_or(0))
            );
        }
        let (whole, fraction) = digits.split_at(usize::try_from(offset).unwrap_or(0));
        return format!("{whole}.{fraction}");
    }
    let keep = usize::try_from(count.min(18 - i32::from(negative))).unwrap_or(1);
    let digits = &digits[..keep];
    let sign = if k + count - 1 < 0 { '-' } else { '+' };
    let (head, tail) = digits.split_at(1);
    let dot = if tail.is_empty() { "" } else { "." };
    format!("{head}{dot}{tail}e{sign}{exp}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_integers() {
        assert_eq!(parse_i64(b"0"), Some(0));
        assert_eq!(parse_i64(b"42"), Some(42));
        assert_eq!(parse_i64(b"-42"), Some(-42));
        assert_eq!(parse_i64(b"9223372036854775807"), Some(i64::MAX));
        assert_eq!(parse_i64(b"-9223372036854775808"), Some(i64::MIN));
    }

    #[test]
    fn rejects_everything_else() {
        for text in [
            &b""[..],
            b"-",
            b"+1",
            b"01",
            b"-0",
            b"1.5",
            b" 1",
            b"1 ",
            b"1e3",
            b"abc",
            b"9223372036854775808",
            b"-9223372036854775809",
        ] {
            assert_eq!(parse_i64(text), None, "{}", String::from_utf8_lossy(text));
        }
    }

    #[test]
    fn doubles_print_like_redis() {
        let cases = [
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (-3.0, "-3"),
            (1.5, "1.5"),
            (0.1, "0.1"),
            (0.300_000_000_000_000_04, "0.30000000000000004"),
            (123_456.789, "123456.789"),
            (0.00123, "0.00123"),
            (1e-7, "1e-7"),
            (1.23e-5, "1.23e-5"),
            (0.000_001, "0.000001"),
            (1e21, "1e+21"),
            (1.5e19, "1.5e+19"),
            (4_611_686_018_427_387_904.0, "4611686018427387904"),
            (1e100, "1e+100"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (-2.5e-10, "-2.5e-10"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
        ];
        for (value, text) in cases {
            assert_eq!(format_double(value), text, "{value:e}");
        }
    }
}
