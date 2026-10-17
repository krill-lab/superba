//! Number formatting shared by the usage, request log and statistics views
//! (UX §2.4: grouped token counts, abbreviated totals).

/// Compact number: 1,284 / 12.9K / 3.4M.
pub fn compact(n: u64) -> String {
    match n {
        0..=9_999 => group_thousands(n),
        10_000..=999_999 => format!("{:.1}K", n as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}B", n as f64 / 1e9),
    }
}

/// `12345` -> `12,345`.
pub fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Rounded "nice" axis maximum and its step for `max` dollars.
pub fn nice_axis(max: f64) -> (f64, f64) {
    if max <= 0.0 || !max.is_finite() {
        return (1.0, 0.5);
    }
    let raw = max / 4.0;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag);
    ((max / step).ceil() * step, step)
}

/// `0.4 MB`, `12.3 MB`, `1,024 MB` (decimal megabytes).
pub fn megabytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb < 100.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{} MB", group_thousands(mb.round() as u64))
    }
}
