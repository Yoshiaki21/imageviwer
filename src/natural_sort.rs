//! Natural ordering for file and folder names.
//!
//! Digit runs compare as numbers so that `img2.jpg` sorts before `img10.jpg`
//! (spec §4). Everything else compares case-insensitively, with the raw
//! characters as a tie-breaker so the order stays total and stable.

use std::cmp::Ordering;

/// Compares two names in natural order.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut left = a.chars().peekable();
    let mut right = b.chars().peekable();

    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(l), Some(r)) => {
                if l.is_ascii_digit() && r.is_ascii_digit() {
                    let (l_digits, l_zeros) = take_number(&mut left);
                    let (r_digits, r_zeros) = take_number(&mut right);
                    match compare_numbers(&l_digits, &r_digits) {
                        Ordering::Equal => {
                            // `01` and `1` are numerically equal; the shorter
                            // (fewer leading zeros) one comes first.
                            match l_zeros.cmp(&r_zeros) {
                                Ordering::Equal => {}
                                other => return other,
                            }
                        }
                        other => return other,
                    }
                } else {
                    left.next();
                    right.next();
                    match lower(l).cmp(&lower(r)) {
                        Ordering::Equal => match l.cmp(&r) {
                            Ordering::Equal => {}
                            // Keep scanning: a case difference only decides the
                            // comparison when the rest of the names are equal.
                            _ => return compare_tail(a, b, l, r),
                        },
                        other => return other,
                    }
                }
            }
        }
    }

    Ordering::Equal
}

/// Compares two names by their natural order, ignoring any directory part.
pub fn natural_cmp_paths(a: &std::path::Path, b: &std::path::Path) -> Ordering {
    let name = |p: &std::path::Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    natural_cmp(&name(a), &name(b))
}

/// Consumes a digit run, returning its digits without leading zeros and the
/// number of leading zeros that were stripped.
fn take_number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> (String, usize) {
    let mut digits = String::new();
    while let Some(c) = chars.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        digits.push(c);
        chars.next();
    }
    let trimmed = digits.trim_start_matches('0');
    let zeros = digits.len() - trimmed.len();
    (trimmed.to_string(), zeros)
}

/// Compares two zero-stripped digit runs as numbers of unbounded length.
fn compare_numbers(a: &str, b: &str) -> Ordering {
    match a.len().cmp(&b.len()) {
        Ordering::Equal => a.cmp(b),
        other => other,
    }
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Resolves a pure case difference: the rest of both names decides first, and
/// only if they are otherwise identical does the character itself break the tie.
fn compare_tail(a: &str, b: &str, l: char, r: char) -> Ordering {
    let lower_a: String = a.chars().flat_map(char::to_lowercase).collect();
    let lower_b: String = b.chars().flat_map(char::to_lowercase).collect();
    match natural_cmp_lowercased(&lower_a, &lower_b) {
        Ordering::Equal => l.cmp(&r),
        other => other,
    }
}

/// `natural_cmp` for inputs that are already lowercased, so it cannot recurse
/// back into [`compare_tail`].
fn natural_cmp_lowercased(a: &str, b: &str) -> Ordering {
    let mut left = a.chars().peekable();
    let mut right = b.chars().peekable();

    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(l), Some(r)) => {
                if l.is_ascii_digit() && r.is_ascii_digit() {
                    let (l_digits, l_zeros) = take_number(&mut left);
                    let (r_digits, r_zeros) = take_number(&mut right);
                    match compare_numbers(&l_digits, &r_digits) {
                        Ordering::Equal => match l_zeros.cmp(&r_zeros) {
                            Ordering::Equal => {}
                            other => return other,
                        },
                        other => return other,
                    }
                } else {
                    left.next();
                    right.next();
                    match l.cmp(&r) {
                        Ordering::Equal => {}
                        other => return other,
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::natural_cmp;
    use std::cmp::Ordering;

    fn sorted(mut names: Vec<&str>) -> Vec<&str> {
        names.sort_by(|a, b| natural_cmp(a, b));
        names
    }

    #[test]
    fn numbers_compare_numerically() {
        assert_eq!(
            sorted(vec!["img10.jpg", "img2.jpg", "img1.jpg"]),
            vec!["img1.jpg", "img2.jpg", "img10.jpg"]
        );
    }

    #[test]
    fn leading_zeros_do_not_change_the_value() {
        assert_eq!(natural_cmp("img007.png", "img7.png"), Ordering::Greater);
        assert_eq!(natural_cmp("img007.png", "img8.png"), Ordering::Less);
    }

    #[test]
    fn very_long_numbers_do_not_overflow() {
        assert_eq!(
            natural_cmp("a99999999999999999999.png", "a100000000000000000000.png"),
            Ordering::Less
        );
    }

    #[test]
    fn case_is_ignored_first() {
        assert_eq!(
            sorted(vec!["Beta.png", "alpha.png", "gamma.png"]),
            vec!["alpha.png", "Beta.png", "gamma.png"]
        );
    }

    #[test]
    fn case_only_difference_is_still_a_total_order() {
        assert_eq!(natural_cmp("A.png", "a.png"), Ordering::Less);
        assert_eq!(natural_cmp("a.png", "a.png"), Ordering::Equal);
    }

    #[test]
    fn prefix_sorts_before_longer_name() {
        assert_eq!(natural_cmp("a.png", "ab.png"), Ordering::Less);
    }
}
