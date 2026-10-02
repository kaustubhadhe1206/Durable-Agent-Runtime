//! Percentile summaries using an HDR histogram.

use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};

use crate::{MetricsError, Result};

// [Phase 5]
/// Summary statistics of a set of samples, in the samples' own unit.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub count: u64,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

// [Phase 5]
/// Summarizes `values`. Percentiles come from an HDR histogram with 3 significant digits
/// (values are rounded to whole units first, so record in a fine unit like µs).
/// Returns `None` if there are no finite samples.
pub fn summarize(values: &[f64]) -> Result<Option<Summary>> {
    // NaN / infinite samples carry no information; ignore them.
    let values: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if values.is_empty() {
        return Ok(None);
    }
    // 3 significant figures: percentile values are accurate to within 0.1%.
    let mut hist = Histogram::<u64>::new(3).map_err(|e| MetricsError::Histogram(e.to_string()))?;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0;
    for &v in &values {
        min = min.min(v);
        max = max.max(v);
        sum += v;
        // Negative samples can't go into the histogram; record them as 0.
        let whole = if v > 0.0 { v.round() as u64 } else { 0 };
        hist.record(whole)
            .map_err(|e| MetricsError::Histogram(e.to_string()))?;
    }
    // The histogram reports the top of a value's bucket, which can sit slightly above the
    // real maximum; clamp so a percentile never exceeds the observed range.
    let pct = |q: f64| (hist.value_at_quantile(q) as f64).clamp(min, max);
    Ok(Some(Summary {
        count: values.len() as u64,
        min,
        max,
        mean: sum / values.len() as f64,
        p50: pct(0.50),
        p95: pct(0.95),
        p99: pct(0.99),
    }))
}

// [Phase 5]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_of_1_to_100() {
        let values: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = summarize(&values).unwrap().unwrap();
        assert_eq!(s.count, 100);
        assert_eq!((s.min, s.max), (1.0, 100.0));
        assert!((s.mean - 50.5).abs() < 1e-9);
        assert_eq!(s.p50, 50.0);
        assert_eq!(s.p95, 95.0);
        assert_eq!(s.p99, 99.0);
    }

    #[test]
    fn single_value() {
        let s = summarize(&[412.0]).unwrap().unwrap();
        assert_eq!((s.p50, s.p99, s.mean), (412.0, 412.0, 412.0));
    }

    #[test]
    fn percentiles_never_exceed_observed_max() {
        let s = summarize(&[60_731.0]).unwrap().unwrap();
        assert!(s.p50 <= s.max && s.p99 <= s.max, "{s:?}");
    }

    #[test]
    fn empty_is_none() {
        assert!(summarize(&[]).unwrap().is_none());
        assert!(summarize(&[f64::NAN]).unwrap().is_none());
    }

    #[test]
    fn large_values_keep_3_significant_digits() {
        let s = summarize(&[1_234_567.0]).unwrap().unwrap();
        assert!(
            (s.p50 - 1_234_567.0).abs() / 1_234_567.0 < 0.001,
            "{}",
            s.p50
        );
    }
}
