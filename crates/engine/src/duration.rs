//! Go-style durations. The runtime's refusal strings quote durations the way
//! Go's time.Duration prints them ("30s", "1m30s", "1h0m0s", "500ms"), and
//! flags and the Input's limits.timeout are written that way ("5s", "1m30s"),
//! so the Rust runtime formats and parses the same syntax.

use std::time::Duration;

/// Formats d as Go's time.Duration.String does.
pub fn format(d: Duration) -> String {
    let ns = d.as_nanos();
    if ns == 0 {
        return "0s".to_string();
    }
    if ns < 1_000 {
        return format!("{ns}ns");
    }
    if ns < 1_000_000 {
        return format!("{}µs", frac(ns, 1_000));
    }
    if ns < 1_000_000_000 {
        return format!("{}ms", frac(ns, 1_000_000));
    }
    let seconds = frac(ns % 60_000_000_000, 1_000_000_000);
    let minutes = (ns / 60_000_000_000) % 60;
    let hours = ns / 3_600_000_000_000;
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, m) => format!("{m}m{seconds}s"),
        (h, m) => format!("{h}h{m}m{seconds}s"),
    }
}

/// The integer part of ns/unit with the fractional digits appended, trailing
/// zeros trimmed - "1", "1.5", "1.25".
fn frac(ns: u128, unit: u128) -> String {
    let whole = ns / unit;
    let rem = ns % unit;
    if rem == 0 {
        return whole.to_string();
    }
    let digits = unit.ilog10() as usize;
    let mut f = format!("{rem:0width$}", width = digits);
    while f.ends_with('0') {
        f.pop();
    }
    format!("{whole}.{f}")
}

/// Parses Go's duration syntax: a sequence of decimal numbers with unit
/// suffixes ("300ms", "1.5h", "2h45m"); units are ns, us (µs), ms, s, m, h.
pub fn parse(s: &str) -> Result<Duration, String> {
    let err = || format!("invalid duration {s:?}");
    let mut rest = s;
    if rest.is_empty() {
        return Err(err());
    }
    let mut total: u128 = 0;
    while !rest.is_empty() {
        let digits = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .count();
        if digits == 0 {
            return Err(err());
        }
        let (number, unit_rest) = rest.split_at(digits);
        let unit_len = unit_rest
            .char_indices()
            .take_while(|(_, c)| !c.is_ascii_digit())
            .map(|(i, c)| i + c.len_utf8())
            .last()
            .ok_or_else(err)?;
        let (unit, next) = unit_rest.split_at(unit_len);
        let scale: u128 = match unit {
            "ns" => 1,
            "us" | "µs" | "μs" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            _ => return Err(err()),
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
        total = total
            .checked_add(whole.checked_mul(scale).ok_or_else(err)?)
            .ok_or_else(err)?;
        if !fraction.is_empty() {
            let f: u128 = fraction.parse().map_err(|_| err())?;
            let denom = 10u128.checked_pow(fraction.len() as u32).ok_or_else(err)?;
            total = total.checked_add(f * scale / denom).ok_or_else(err)?;
        }
        rest = next;
    }
    Ok(Duration::from_nanos(
        u64::try_from(total).map_err(|_| err())?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_go() {
        let cases = [
            (Duration::ZERO, "0s"),
            (Duration::from_secs(30), "30s"),
            (Duration::from_secs(90), "1m30s"),
            (Duration::from_secs(60), "1m0s"),
            (Duration::from_secs(3600), "1h0m0s"),
            (Duration::from_secs(3690), "1h1m30s"),
            (Duration::from_millis(500), "500ms"),
            (Duration::from_millis(1500), "1.5s"),
            (Duration::from_micros(1500), "1.5ms"),
            (Duration::from_nanos(500), "500ns"),
            (Duration::from_secs_f64(61.25), "1m1.25s"),
        ];
        for (d, want) in cases {
            assert_eq!(format(d), want, "format({d:?})");
        }
    }

    #[test]
    fn parses_like_go() {
        let cases = [
            ("30s", Duration::from_secs(30)),
            ("1m30s", Duration::from_secs(90)),
            ("1.5h", Duration::from_secs(5400)),
            ("300ms", Duration::from_millis(300)),
            ("2h45m", Duration::from_secs(9900)),
            ("1µs", Duration::from_micros(1)),
        ];
        for (s, want) in cases {
            assert_eq!(parse(s).unwrap(), want, "parse({s:?})");
        }
        for bad in ["", "5", "s", "5x", "-5s"] {
            assert!(parse(bad).is_err(), "parse({bad:?}) should fail");
        }
    }
}
