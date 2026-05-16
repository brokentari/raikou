//! Extended-precision numbers that behave like the x87 `long double` of Redis.

const MAX_EXPONENT: i32 = 16383;

/// A finite value `mantissa * 2^exponent`; the mantissa is zero or has its top bit set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LongDouble {
    negative: bool,
    mantissa: u64,
    exponent: i32,
}

/// The result of reading a number from text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Parsed {
    Finite(LongDouble),
    Infinite,
}

fn bit_length(value: u128) -> u32 {
    128 - value.leading_zeros()
}

/// Rounds `value * 2^exponent` to 64 bits, to nearest and ties to even.
fn round_to_64(value: u128, sticky: bool, exponent: i32) -> (u64, i32) {
    let bits = bit_length(value);
    if bits <= 64 {
        let lift = 64 - bits;
        return (
            u64::try_from(value << lift).unwrap_or(0),
            exponent - i32::try_from(lift).unwrap_or(0),
        );
    }
    let shift = bits - 64;
    let low = value & ((1u128 << shift) - 1);
    let half = 1u128 << (shift - 1);
    let mut mantissa = value >> shift;
    let mut exponent = exponent + i32::try_from(shift).unwrap_or(0);
    if low > half || (low == half && (sticky || mantissa & 1 == 1)) {
        mantissa += 1;
        if mantissa == 1u128 << 64 {
            mantissa = 1u128 << 63;
            exponent += 1;
        }
    }
    (u64::try_from(mantissa).unwrap_or(0), exponent)
}

impl LongDouble {
    const ZERO: Self = Self {
        negative: false,
        mantissa: 0,
        exponent: 0,
    };

    fn from_f64(value: f64) -> Self {
        if value == 0.0 {
            return Self {
                negative: value.is_sign_negative(),
                ..Self::ZERO
            };
        }
        let bits = value.to_bits();
        let raw_exponent = i32::try_from((bits >> 52) & 0x7ff).unwrap_or(0);
        let fraction = bits & ((1 << 52) - 1);
        let (significand, exponent) = if raw_exponent == 0 {
            (fraction, -1074)
        } else {
            (fraction | (1 << 52), raw_exponent - 1075)
        };
        let lift = significand.leading_zeros();
        Self {
            negative: value.is_sign_negative(),
            mantissa: significand << lift,
            exponent: exponent - i32::try_from(lift).unwrap_or(0),
        }
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "only a fallback for values outside the exact range"
    )]
    fn to_f64(self) -> f64 {
        let magnitude = self.mantissa as f64 * 2f64.powi(self.exponent);
        if self.negative { -magnitude } else { magnitude }
    }

    fn exact(negative: bool, digits: u128, power: i32) -> Option<Self> {
        if digits == 0 {
            return Some(Self {
                negative,
                ..Self::ZERO
            });
        }
        let (mantissa, exponent) = if power >= 0 {
            let scaled = digits.checked_mul(10u128.checked_pow(u32::try_from(power).ok()?)?)?;
            round_to_64(scaled, false, 0)
        } else {
            let divisor = 10u128.checked_pow(u32::try_from(-power).ok()?)?;
            let mut accumulated = digits / divisor;
            let mut remainder = digits % divisor;
            let mut fractional_bits = 0;
            while bit_length(accumulated) < 66 {
                remainder <<= 1;
                accumulated <<= 1;
                if remainder >= divisor {
                    remainder -= divisor;
                    accumulated |= 1;
                }
                fractional_bits += 1;
            }
            round_to_64(accumulated, remainder != 0, -fractional_bits)
        };
        Some(Self {
            negative,
            mantissa,
            exponent,
        })
    }

    fn parse_hex(negative: bool, text: &str) -> Option<Parsed> {
        let (number, exponent) = match text.find(['p', 'P']) {
            Some(index) => (&text[..index], Some(&text[index + 1..])),
            None => (text, None),
        };
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        if (whole.is_empty() && fraction.is_empty())
            || !whole
                .chars()
                .chain(fraction.chars())
                .all(|c| c.is_ascii_hexdigit())
        {
            return None;
        }
        let mut power: i32 = match exponent {
            None => 0,
            Some(exponent) => {
                let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
                if digits.is_empty()
                    || digits.len() > 6
                    || !digits.bytes().all(|b| b.is_ascii_digit())
                {
                    return None;
                }
                exponent.parse().ok()?
            }
        };
        power -= 4 * i32::try_from(fraction.len()).ok()?;
        let digits = u128::from_str_radix(&format!("{whole}{fraction}"), 16).ok()?;
        if digits == 0 {
            return Some(Parsed::Finite(Self {
                negative,
                ..Self::ZERO
            }));
        }
        let (mantissa, exponent) = round_to_64(digits, false, power);
        // shortcut: results below the x87 normal range are not modeled
        (exponent + 63 <= MAX_EXPONENT && exponent >= -16445).then_some(Parsed::Finite(Self {
            negative,
            mantissa,
            exponent,
        }))
    }

    /// Reads a decimal number or an infinity; `None` means the text is not a number.
    pub(crate) fn parse(text: &[u8]) -> Option<Parsed> {
        let text = std::str::from_utf8(text).ok()?;
        let (negative, body) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text.strip_prefix('+').unwrap_or(text)),
        };
        if body.eq_ignore_ascii_case("inf") || body.eq_ignore_ascii_case("infinity") {
            return Some(Parsed::Infinite);
        }
        if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            return Self::parse_hex(negative, hex);
        }
        let (number, exponent) = match body.find(['e', 'E']) {
            Some(index) => (&body[..index], Some(&body[index + 1..])),
            None => (body, None),
        };
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        let all_digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
        if !all_digits(whole) || !all_digits(fraction) || (whole.is_empty() && fraction.is_empty())
        {
            return None;
        }
        let mut power: i64 = match exponent {
            None => 0,
            Some(exponent) => {
                let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
                if digits.is_empty() || !all_digits(digits) || digits.len() > 9 {
                    return None;
                }
                exponent.parse().ok()?
            }
        };
        power -= i64::try_from(fraction.len()).ok()?;
        let joined = format!("{whole}{fraction}");
        let significant = joined.trim_start_matches('0');
        let trimmed = significant.trim_end_matches('0');
        power += i64::try_from(significant.len() - trimmed.len()).ok()?;
        if trimmed.is_empty() {
            return Some(Parsed::Finite(Self {
                negative,
                ..Self::ZERO
            }));
        }
        let magnitude = power + i64::try_from(trimmed.len()).ok()?;
        if !(-4950..=4932).contains(&magnitude) {
            return None;
        }
        let exact = trimmed
            .parse::<u128>()
            .ok()
            .zip(i32::try_from(power).ok())
            .and_then(|(digits, power)| Self::exact(negative, digits, power));
        if let Some(value) = exact {
            return Some(Parsed::Finite(value));
        }
        // shortcut: numbers beyond 38 digits or 10^38 are read with double precision
        let value: f64 = text.parse().ok()?;
        value
            .is_finite()
            .then(|| Parsed::Finite(Self::from_f64(value)))
    }

    /// Adds two values; `None` means the sum overflows.
    pub(crate) fn checked_add(self, other: Self) -> Option<Self> {
        if self.mantissa == 0 {
            let both_negative = self.negative && other.negative;
            return Some(if other.mantissa == 0 {
                Self {
                    negative: both_negative,
                    ..Self::ZERO
                }
            } else {
                other
            });
        }
        if other.mantissa == 0 {
            return Some(self);
        }
        let (big, small) = if (self.exponent, self.mantissa) >= (other.exponent, other.mantissa) {
            (self, other)
        } else {
            (other, self)
        };
        let diff = u32::try_from(big.exponent - small.exponent).unwrap_or(u32::MAX);
        let large = u128::from(big.mantissa) << 62;
        let full = u128::from(small.mantissa) << 62;
        let (mut shifted, lost) = if diff >= 128 {
            (0, full != 0)
        } else {
            (full >> diff, full & ((1u128 << diff) - 1) != 0)
        };
        if lost {
            shifted |= 1;
        }
        let sum = if big.negative == small.negative {
            large + shifted
        } else {
            large - shifted
        };
        if sum == 0 {
            return Some(Self::ZERO);
        }
        let (mantissa, exponent) = round_to_64(sum, false, big.exponent - 62);
        (exponent + 63 <= MAX_EXPONENT).then_some(Self {
            negative: big.negative,
            mantissa,
            exponent,
        })
    }

    /// Formats like C `%.17Lf` with the trailing zeros removed, as Redis does.
    pub(crate) fn to_human_string(self) -> String {
        if self.mantissa == 0 {
            return "0".to_owned();
        }
        let sign = if self.negative { "-" } else { "" };
        let mantissa = u128::from(self.mantissa);
        let text = if self.exponent >= 0 {
            match u32::try_from(self.exponent)
                .ok()
                .filter(|shift| *shift <= 64)
            {
                Some(shift) => (mantissa << shift).to_string(),
                // shortcut: values beyond 2^128 are printed from their double approximation
                None => format!("{:.0}", self.to_f64().abs()),
            }
        } else {
            match u32::try_from(-self.exponent)
                .ok()
                .filter(|shift| *shift <= 120)
            {
                Some(shift) => fixed_17(mantissa, shift),
                None => format!("{:.17}", self.to_f64().abs()),
            }
        };
        let trimmed = if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.')
        } else {
            &text
        };
        if trimmed.is_empty() || trimmed == "0" {
            "0".to_owned()
        } else {
            format!("{sign}{trimmed}")
        }
    }
}

fn fixed_17(mantissa: u128, shift: u32) -> String {
    let mask = (1u128 << shift) - 1;
    let mut whole = mantissa >> shift;
    let mut fraction = mantissa & mask;
    let mut digits = [0u8; 17];
    for digit in &mut digits {
        fraction *= 10;
        *digit = u8::try_from(fraction >> shift).unwrap_or(0);
        fraction &= mask;
    }
    let half = 1u128 << (shift - 1);
    if fraction > half || (fraction == half && digits[16] % 2 == 1) {
        let mut index = 17;
        loop {
            if index == 0 {
                whole += 1;
                break;
            }
            index -= 1;
            if digits[index] == 9 {
                digits[index] = 0;
            } else {
                digits[index] += 1;
                break;
            }
        }
    }
    let fraction: String = digits
        .iter()
        .map(|digit| char::from(b'0' + digit))
        .collect();
    format!("{whole}.{fraction}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> LongDouble {
        match LongDouble::parse(text.as_bytes()) {
            Some(Parsed::Finite(value)) => value,
            other => panic!("{text}: {other:?}"),
        }
    }

    fn sum(left: &str, right: &str) -> String {
        value(left)
            .checked_add(value(right))
            .expect("finite")
            .to_human_string()
    }

    #[test]
    fn decimal_sums_look_the_way_users_expect() {
        assert_eq!(sum("0.1", "0.2"), "0.3");
        assert_eq!(sum("10.5", "0.1"), "10.6");
        assert_eq!(sum("3.0e3", "2"), "3002");
        assert_eq!(sum("1e21", "0"), "1000000000000000000000");
        assert_eq!(sum("1e-7", "0"), "0.0000001");
        assert_eq!(sum("-1.5", "1.5"), "0");
        assert_eq!(sum("-0", "0"), "0");
        assert_eq!(sum("5", "-7.25"), "-2.25");
        assert_eq!(sum(".5", "5."), "5.5");
    }

    #[test]
    fn hexadecimal_floats() {
        assert_eq!(sum("0x10", "0"), "16");
        assert_eq!(sum("0x1.8p1", "0"), "3");
        assert_eq!(sum("-0xA", "0.5"), "-9.5");
        assert_eq!(sum("0x.8", "0"), "0.5");
    }

    #[test]
    fn precision_is_64_bits() {
        assert_eq!(sum("9007199254740993", "1"), "9007199254740994");
        assert_eq!(sum("18446744073709551615", "1"), "18446744073709551616");
    }

    #[test]
    fn large_magnitudes_show_rounding_noise_like_x87() {
        assert_eq!(
            value("100000.1").to_human_string(),
            "100000.10000000000000142"
        );
        assert_eq!(value("0.1").to_human_string(), "0.1");
    }

    #[test]
    fn rejects_text_that_is_not_a_number() {
        for text in [
            "", "abc", "1.2.3", "1e", "e5", ".", "-", "0x", "0xg", " 1", "1 ", "nan", "1e5000",
            "--1",
        ] {
            assert_eq!(LongDouble::parse(text.as_bytes()), None, "{text:?}");
        }
    }

    #[test]
    fn infinity_is_recognized() {
        for text in ["inf", "-inf", "+Infinity", "INF"] {
            assert_eq!(
                LongDouble::parse(text.as_bytes()),
                Some(Parsed::Infinite),
                "{text}"
            );
        }
    }

    #[test]
    fn overflow_is_reported() {
        let near_max = LongDouble {
            negative: false,
            mantissa: 1 << 63,
            exponent: 16320,
        };
        assert_eq!(near_max.checked_add(near_max), None);
        assert!(near_max.checked_add(LongDouble::ZERO).is_some());
    }
}
