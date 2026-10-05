// Human-readable sizes and rates for log lines.

use std::fmt;
use std::time::Duration;

pub struct Bytes(pub u64);

impl fmt::Display for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
        let mut value = self.0 as f64;
        let mut unit = 0;
        while value >= 1024.0 && unit < UNITS.len() - 1 {
            value /= 1024.0;
            unit += 1;
        }
        if unit == 0 { write!(f, "{} B", self.0) } else { write!(f, "{value:.1} {}", UNITS[unit]) }
    }
}

/// `bytes` transferred over `elapsed`, as e.g. "4.2 MiB/s".
pub struct Rate {
    pub bytes: u64,
    pub elapsed: Duration,
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.elapsed.as_secs_f64();
        if secs <= 0.0 {
            return write!(f, "n/a");
        }
        write!(f, "{}/s", Bytes((self.bytes as f64 / secs) as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_and_rates() {
        assert_eq!(Bytes(512).to_string(), "512 B");
        assert_eq!(Bytes(5 * 1024 * 1024 + 512 * 1024).to_string(), "5.5 MiB");
        assert_eq!(Rate { bytes: 8 * 1024 * 1024, elapsed: Duration::from_secs(2) }.to_string(), "4.0 MiB/s");
        assert_eq!(Rate { bytes: 1, elapsed: Duration::ZERO }.to_string(), "n/a");
    }
}
