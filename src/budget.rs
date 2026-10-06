//! Size literals and formatting.
//!
//! Unit policy (deliberately unambiguous, and unit-tested):
//!   `KB = 1000`, `MB = 1000^2`, `GB = 1000^3`   (decimal)
//!   `KiB = 1024`, `MiB = 1024^2`, `GiB = 1024^3` (binary)
//! A bare number is bytes. Fractions are allowed (`1.5MB`).

/// Parse a size literal such as `1048576`, `1.5MB`, `900 KB`, `2GiB`.
pub fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("empty size".to_string());
    }
    let up = t.to_ascii_uppercase();
    // Split into a numeric head and an alphabetic unit tail.
    let split = up
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(up.len());
    let (num, unit) = up.split_at(split);
    let num = num.trim();
    let unit = unit.trim();
    if num.is_empty() {
        return Err(format!("size {s:?} has no number"));
    }
    let value: f64 = num
        .parse()
        .map_err(|_| format!("size {s:?} has an invalid number {num:?}"))?;
    if !value.is_finite() || value < 0.0 {
        return Err(format!("size {s:?} is not a positive number"));
    }
    let mult: f64 = match unit {
        "" | "B" => 1.0,
        "K" | "KB" => 1_000.0,
        "M" | "MB" => 1_000_000.0,
        "G" | "GB" => 1_000_000_000.0,
        "KIB" => 1024.0,
        "MIB" => 1024.0 * 1024.0,
        "GIB" => 1024.0 * 1024.0 * 1024.0,
        other => return Err(format!("size {s:?} has an unknown unit {other:?}")),
    };
    let bytes = value * mult;
    if bytes > u64::MAX as f64 {
        return Err(format!("size {s:?} overflows u64"));
    }
    Ok(bytes as u64)
}

/// `2548384` -> `2.43 MB (2548384 bytes)`.
#[must_use]
pub fn fmt_bytes(n: u64) -> String {
    let f = n as f64;
    let (v, unit) = if f >= 1e9 {
        (f / 1e9, "GB")
    } else if f >= 1e6 {
        (f / 1e6, "MB")
    } else if f >= 1e3 {
        (f / 1e3, "KB")
    } else {
        (f, "B")
    };
    format!("{v:.2} {unit} ({n} bytes)")
}

/// Signed percentage of `after` relative to `before`, e.g. `-51.8`.
#[must_use]
pub fn pct_delta(before: u64, after: u64) -> Option<f64> {
    if before == 0 {
        return None;
    }
    Some((after as f64 - before as f64) * 100.0 / before as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_bytes() {
        assert_eq!(parse_size("12345").unwrap(), 12345);
        assert_eq!(parse_size("0").unwrap(), 0);
        assert_eq!(parse_size(" 42 ").unwrap(), 42);
    }

    #[test]
    fn parse_decimal_units() {
        assert_eq!(parse_size("1KB").unwrap(), 1000);
        assert_eq!(parse_size("1.5MB").unwrap(), 1_500_000);
        assert_eq!(parse_size("2 GB").unwrap(), 2_000_000_000);
    }

    #[test]
    fn parse_binary_units() {
        assert_eq!(parse_size("1KiB").unwrap(), 1024);
        assert_eq!(parse_size("1.5MiB").unwrap(), 1_572_864);
        assert_eq!(parse_size("2GiB").unwrap(), 2_147_483_648);
    }

    #[test]
    fn parse_rejects_junk() {
        assert!(parse_size("").is_err());
        assert!(parse_size("MB").is_err());
        assert!(parse_size("12XB").is_err());
        assert!(parse_size("-5").is_err());
        assert!(parse_size("abc").is_err());
    }

    #[test]
    fn format_is_human_readable() {
        assert_eq!(fmt_bytes(512), "512.00 B (512 bytes)");
        assert_eq!(fmt_bytes(2548384), "2.55 MB (2548384 bytes)");
    }

    #[test]
    fn pct_handles_zero_baseline() {
        assert_eq!(pct_delta(0, 10), None);
        assert_eq!(pct_delta(100, 50), Some(-50.0));
    }
}
