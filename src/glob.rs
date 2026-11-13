const MAX_NESTING: u32 = 1000;

#[must_use]
pub(crate) fn matches(pattern: &[u8], string: &[u8]) -> bool {
    matches_nested(pattern, 0, string, 0, 0, &mut false)
}

fn matches_nested(
    pattern: &[u8],
    mut p: usize,
    string: &[u8],
    mut s: usize,
    nesting: u32,
    skip_longer: &mut bool,
) -> bool {
    if nesting > MAX_NESTING {
        return false;
    }
    while p < pattern.len() && s < string.len() {
        match pattern[p] {
            b'*' => {
                while pattern.get(p + 1) == Some(&b'*') {
                    p += 1;
                }
                if p + 1 == pattern.len() {
                    return true;
                }
                while s < string.len() {
                    if matches_nested(pattern, p + 1, string, s, nesting + 1, skip_longer) {
                        return true;
                    }
                    if *skip_longer {
                        return false;
                    }
                    s += 1;
                }
                // the rest of the pattern fits nowhere, so earlier stars cannot help either
                *skip_longer = true;
                return false;
            }
            b'?' => s += 1,
            b'[' => {
                let (next_p, matched) = bracket(pattern, p + 1, string[s]);
                if !matched {
                    return false;
                }
                p = next_p;
                s += 1;
            }
            byte => {
                let literal = if byte == b'\\' && p + 1 < pattern.len() {
                    p += 1;
                    pattern[p]
                } else {
                    byte
                };
                if literal != string[s] {
                    return false;
                }
                s += 1;
            }
        }
        p += 1;
        if s == string.len() {
            while p < pattern.len() && pattern[p] == b'*' {
                p += 1;
            }
            break;
        }
    }
    p == pattern.len() && s == string.len()
}

/// Reads a `[...]` class starting after the bracket; returns the index of its last byte.
fn bracket(pattern: &[u8], mut p: usize, byte: u8) -> (usize, bool) {
    let negated = pattern.get(p) == Some(&b'^');
    if negated {
        p += 1;
    }
    let mut matched = false;
    loop {
        let rest = pattern.len() - p;
        if rest >= 2 && pattern[p] == b'\\' {
            p += 1;
            matched |= pattern[p] == byte;
        } else if rest == 0 {
            p -= 1;
            break;
        } else if pattern[p] == b']' {
            break;
        } else if rest >= 3 && pattern[p + 1] == b'-' {
            let (low, high) = (
                pattern[p].min(pattern[p + 2]),
                pattern[p].max(pattern[p + 2]),
            );
            p += 2;
            matched |= (low..=high).contains(&byte);
        } else {
            matched |= pattern[p] == byte;
        }
        p += 1;
    }
    (p, matched != negated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(pattern: &str, string: &str, expected: bool) {
        assert_eq!(
            matches(pattern.as_bytes(), string.as_bytes()),
            expected,
            "{pattern} vs {string}"
        );
    }

    #[test]
    fn literals_and_wildcards() {
        check("hello", "hello", true);
        check("hello", "hell", false);
        check("h?llo", "hello", true);
        check("h?llo", "hllo", false);
        check("h*llo", "heeeello", true);
        check("h*llo", "hllo", true);
        check("*", "", false);
        check("*", "anything", true);
        check("a*b*c", "axxbyyc", true);
        check("a*b*c", "axxbyy", false);
        check("**a", "xa", true);
        check("a*", "a", true);
        check("*a*", "bab", true);
    }

    #[test]
    fn classes() {
        check("h[ae]llo", "hello", true);
        check("h[ae]llo", "hallo", true);
        check("h[ae]llo", "hillo", false);
        check("h[^e]llo", "hallo", true);
        check("h[^e]llo", "hello", false);
        check("h[a-b]llo", "hallo", true);
        check("h[a-b]llo", "hcllo", false);
        check("h[b-a]llo", "hallo", true);
        check("[\\]]", "]", true);
        check("h[a", "ha", true);
    }

    #[test]
    fn escapes() {
        check("h\\*llo", "h*llo", true);
        check("h\\*llo", "hello", false);
        check("h\\?llo", "h?llo", true);
        check("h\\\\llo", "h\\llo", true);
    }

    #[test]
    fn empty_cases_and_binary() {
        check("", "", true);
        check("", "a", false);
        check("a", "", false);
        check("[\\x00]", "\0", false);
        assert!(matches(b"a\x00*", b"a\x00b"));
    }

    #[test]
    fn deep_patterns_stop_instead_of_overflowing() {
        let pattern = "a*".repeat(2000);
        let _ = matches(pattern.as_bytes(), "a".repeat(2000).as_bytes());
    }
}
