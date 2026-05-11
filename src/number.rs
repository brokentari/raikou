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
}
