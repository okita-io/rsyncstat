//! Human-readable byte sizes.

/// Format `n` as B, KiB, MiB or GiB with `decimals` places above one KiB.
pub fn format_bytes(n: u64, decimals: usize) -> String {
    const K: f64 = 1024.0;
    let n = n as f64;
    if n >= K * K * K {
        format!("{:.decimals$} GiB", n / (K * K * K))
    } else if n >= K * K {
        format!("{:.decimals$} MiB", n / (K * K))
    } else if n >= K {
        format!("{:.decimals$} KiB", n / K)
    } else {
        format!("{n:.0} B")
    }
}

#[cfg(test)]
mod tests {
    use super::format_bytes;

    #[test]
    fn picks_the_largest_unit() {
        assert_eq!(format_bytes(0, 1), "0 B");
        assert_eq!(format_bytes(1023, 1), "1023 B");
        assert_eq!(format_bytes(1536, 1), "1.5 KiB");
        assert_eq!(format_bytes(3 * 1024 * 1024, 2), "3.00 MiB");
        assert_eq!(format_bytes(5 * 1024 * 1024 * 1024, 1), "5.0 GiB");
    }
}
