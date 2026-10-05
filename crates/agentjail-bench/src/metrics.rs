//! Latency aggregation and JSON output.
//!
//! Percentiles computed on a sorted vector — at a few thousand samples
//! this is cheaper than an HDR histogram and has zero deps.

use serde::Serialize;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Serialize)]
pub struct LatencyStats {
    pub min: u64,
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub max: u64,
    pub mean: u64,
    pub stddev: u64,
}

impl LatencyStats {
    /// Compute percentiles from a slice of latencies (microseconds).
    /// Returns `None` if the slice is empty — a scenario with zero
    /// successful iterations has no latency to report.
    pub fn from_micros(samples: &[u64]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted: Vec<u64> = samples.to_vec();
        sorted.sort_unstable();

        let mean = sorted.iter().sum::<u64>() as f64 / sorted.len() as f64;
        let variance = sorted
            .iter()
            .map(|&x| {
                let d = x as f64 - mean;
                d * d
            })
            .sum::<f64>()
            / sorted.len() as f64;

        Some(Self {
            min: sorted[0],
            p50: pct(&sorted, 0.50),
            p95: pct(&sorted, 0.95),
            p99: pct(&sorted, 0.99),
            max: *sorted.last().unwrap(),
            mean: mean as u64,
            stddev: variance.sqrt() as u64,
        })
    }
}

fn pct(sorted: &[u64], p: f64) -> u64 {
    // Nearest-rank percentile. Close enough at our sample sizes; we're
    // not trying to compete with HdrHistogram precision.
    let idx = ((sorted.len() as f64) * p).ceil() as usize;
    let idx = idx.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub scenario: String,
    pub concurrency: usize,
    pub iters: usize,
    pub warmup: usize,
    pub errors: usize,
    pub wall_clock_s: f64,
    pub throughput_per_s: f64,
    pub latency_us: Option<LatencyStats>,
    /// Scenario-specific extras (e.g. `clone_method` for live-fork).
    pub extra: serde_json::Value,
    pub env: crate::env::EnvInfo,
    pub timestamp_unix: u64,
}

impl Report {
    pub fn from_samples(
        scenario: &str,
        concurrency: usize,
        iters: usize,
        warmup: usize,
        wall: Duration,
        latencies_us: &[u64],
        errors: usize,
        extra: serde_json::Value,
    ) -> Self {
        let successful = latencies_us.len();
        let wall_s = wall.as_secs_f64();
        let throughput = if wall_s > 0.0 {
            successful as f64 / wall_s
        } else {
            0.0
        };

        Self {
            scenario: scenario.to_string(),
            concurrency,
            iters,
            warmup,
            errors,
            wall_clock_s: wall_s,
            throughput_per_s: throughput,
            latency_us: LatencyStats::from_micros(latencies_us),
            extra,
            env: crate::env::EnvInfo::capture(),
            timestamp_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }

    pub fn print_summary(&self) {
        println!(
            "scenario={}  concurrency={}  iters={}  errors={}",
            self.scenario, self.concurrency, self.iters, self.errors
        );
        println!(
            "  wall={:.2}s  throughput={:.1}/s",
            self.wall_clock_s, self.throughput_per_s
        );
        if let Some(ref l) = self.latency_us {
            println!(
                "  latency_us  min={}  p50={}  p95={}  p99={}  max={}  mean={}  stddev={}",
                l.min, l.p50, l.p95, l.p99, l.max, l.mean, l.stddev
            );
        } else {
            println!("  latency_us  (no successful iterations)");
        }
        if !self.extra.is_null() {
            println!("  extra={}", self.extra);
        }
    }

    pub fn write_json(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_samples_yield_none() {
        assert!(LatencyStats::from_micros(&[]).is_none());
    }

    #[test]
    fn percentiles_on_1_to_100() {
        let samples: Vec<u64> = (1..=100).collect();
        let s = LatencyStats::from_micros(&samples).unwrap();
        assert_eq!(s.min, 1);
        assert_eq!(s.max, 100);
        assert_eq!(s.p50, 50);
        assert_eq!(s.p95, 95);
        assert_eq!(s.p99, 99);
    }

    #[test]
    fn single_sample() {
        let s = LatencyStats::from_micros(&[42]).unwrap();
        assert_eq!(s.min, 42);
        assert_eq!(s.max, 42);
        assert_eq!(s.p50, 42);
        assert_eq!(s.p99, 42);
        assert_eq!(s.stddev, 0);
    }
}
