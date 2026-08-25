//! A Kubernetes resource.Quantity subset: enough to parse the Input's
//! limits.memory ("128Mi", "1Gi", plain bytes) and to print a byte count the
//! way Go's BinarySI canonical form does, so the refusal strings match the
//! Go runtime's.

const BINARY: &[(&str, u32)] = &[
    ("Ki", 10),
    ("Mi", 20),
    ("Gi", 30),
    ("Ti", 40),
    ("Pi", 50),
    ("Ei", 60),
];
const DECIMAL: &[(&str, u64)] = &[
    ("k", 1_000),
    ("M", 1_000_000),
    ("G", 1_000_000_000),
    ("T", 1_000_000_000_000),
    ("P", 1_000_000_000_000_000),
    ("E", 1_000_000_000_000_000_000),
];

/// Parses a quantity into bytes, rounding a fractional value up like
/// Quantity.Value does. Negative values parse (to be refused as non-positive
/// by the caller, with the same message the Go runtime uses).
pub fn parse(s: &str) -> Result<i128, String> {
    let err = || format!("cannot parse quantity {s:?}");
    let (negative, rest) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let digits = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .count();
    if digits == 0 {
        return Err(err());
    }
    let (number, suffix) = rest.split_at(digits);
    let scale: u128 = if suffix.is_empty() {
        1
    } else if let Some((_, shift)) = BINARY.iter().find(|(sfx, _)| *sfx == suffix) {
        1u128 << shift
    } else if let Some((_, mult)) = DECIMAL.iter().find(|(sfx, _)| *sfx == suffix) {
        u128::from(*mult)
    } else {
        return Err(err());
    };
    let (whole, fraction) = match number.split_once('.') {
        None => (number, ""),
        Some((w, f)) => (w, f),
    };
    if whole.is_empty() && fraction.is_empty() {
        return Err(err());
    }
    let whole: u128 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| err())?
    };
    let mut value = whole.checked_mul(scale).ok_or_else(err)?;
    if !fraction.is_empty() {
        let f: u128 = fraction.parse().map_err(|_| err())?;
        let denom = 10u128.checked_pow(fraction.len() as u32).ok_or_else(err)?;
        value = value
            .checked_add((f * scale).div_ceil(denom))
            .ok_or_else(err)?;
    }
    let value = i128::try_from(value).map_err(|_| err())?;
    Ok(if negative { -value } else { value })
}

/// Formats bytes as Go's BinarySI canonical form: the largest binary suffix
/// that divides evenly, else plain bytes.
pub fn format_binary_si(bytes: u64) -> String {
    for (suffix, shift) in BINARY.iter().rev() {
        let unit = 1u64 << shift;
        if bytes >= unit && bytes.is_multiple_of(unit) {
            return format!("{}{suffix}", bytes / unit);
        }
    }
    bytes.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let cases = [
            ("128Mi", 128 << 20),
            ("1Gi", 1 << 30),
            ("1024", 1024),
            ("1.5Gi", 3 << 29),
            ("500M", 500_000_000),
            ("-1Gi", -(1 << 30)),
        ];
        for (s, want) in cases {
            assert_eq!(parse(s).unwrap(), i128::from(want as i64), "parse({s:?})");
        }
        for bad in ["", "Mi", "1Zi", "x"] {
            assert!(parse(bad).is_err(), "parse({bad:?}) should fail");
        }
    }

    #[test]
    fn formats() {
        assert_eq!(format_binary_si(512 << 20), "512Mi");
        assert_eq!(format_binary_si(1 << 30), "1Gi");
        assert_eq!(format_binary_si(1000), "1000");
    }
}
