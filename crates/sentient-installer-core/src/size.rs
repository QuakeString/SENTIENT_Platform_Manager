// Copyright © 2016-2026 The SENTIENT Authors
//! Sizes in words, one way everywhere (binary units, as disk tools show):
//! whole MB below 1 GB (420 MB), GB with one decimal below 10 GB (6.7 GB),
//! whole GB from 10 GB (94 GB), and TB the same way.

/// `bytes` as SPM shows a size.
pub fn label(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    let one_or_whole = |v: f64, unit: &str| if v < 10.0 { format!("{v:.1} {unit}") } else { format!("{v:.0} {unit}") };
    if b >= K * K * K * K {
        one_or_whole(b / (K * K * K * K), "TB")
    } else if b >= K * K * K {
        one_or_whole(b / (K * K * K), "GB")
    } else if b >= K * K {
        format!("{:.0} MB", b / (K * K))
    } else if b >= K {
        format!("{:.0} KB", b / K)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::label;

    #[test]
    fn sizes_read_as_in_the_mockup() {
        const M: u64 = 1 << 20;
        const G: u64 = 1 << 30;
        assert_eq!(label(420 * M), "420 MB");
        assert_eq!(label(6 * G + 700 * M), "6.7 GB");
        assert_eq!(label(94 * G), "94 GB");
        assert_eq!(label(1500), "1 KB");
        assert_eq!(label(12), "12 B");
        assert_eq!(label(2 * 1024 * G), "2.0 TB");
    }
}
