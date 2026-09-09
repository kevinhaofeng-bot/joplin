use std::path::Path;

use serde::Serialize;

/// One fixed bucket for every microsecond through the strictest 16ms gate,
/// plus an overflow bucket.  This keeps the histogram bounded while ensuring
/// a slow sample can never be hidden by quantization.
pub const FIXED_HISTOGRAM_BUCKETS: usize = 16_002;
const OVERFLOW_BUCKET: usize = 16_001;
pub const TEXTURE_BYTES_LIMIT: usize = 48 * 1024 * 1024;
pub const LAYOUT_CACHE_BYTES_LIMIT: usize = 16 * 1024 * 1024;
pub const UNDO_BYTES_LIMIT: usize = 16 * 1024 * 1024;
pub const TRANSACTION_P95_US_LIMIT: u64 = 8_000;
pub const RENDER_COMMIT_P95_US_LIMIT: u64 = 16_000;

#[derive(Clone, Debug)]
pub struct FixedHistogram {
    buckets: [u64; FIXED_HISTOGRAM_BUCKETS],
    count: u64,
}

impl Default for FixedHistogram {
    fn default() -> Self {
        Self {
            buckets: [0; FIXED_HISTOGRAM_BUCKETS],
            count: 0,
        }
    }
}

impl FixedHistogram {
    pub fn observe_us(&mut self, sample: u64) {
        let bucket = usize::try_from(sample)
            .unwrap_or(OVERFLOW_BUCKET)
            .min(OVERFLOW_BUCKET);
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
        self.count = self.count.saturating_add(1);
    }

    pub fn p95_us(&self) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let target = (self.count.saturating_mul(95).saturating_add(99)) / 100;
        let mut cumulative = 0u64;
        for (bucket, count) in self.buckets.iter().enumerate() {
            cumulative = cumulative.saturating_add(*count);
            if cumulative >= target {
                return bucket as u64;
            }
        }
        OVERFLOW_BUCKET as u64
    }

    pub fn retained_sample_count(&self) -> usize {
        0
    }

    pub const fn bucket_count(&self) -> usize {
        FIXED_HISTOGRAM_BUCKETS
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Diagnostics {
    pub texture_bytes: usize,
    pub layout_cache_bytes: usize,
    pub undo_bytes: usize,
    pub transaction_p95_us: u64,
    pub render_commit_p95_us: u64,
}

impl Diagnostics {
    pub const fn from_values(
        texture_bytes: usize,
        layout_cache_bytes: usize,
        undo_bytes: usize,
        transaction_p95_us: u64,
        render_commit_p95_us: u64,
    ) -> Self {
        Self {
            texture_bytes,
            layout_cache_bytes,
            undo_bytes,
            transaction_p95_us,
            render_commit_p95_us,
        }
    }

    pub fn gates_pass(&self) -> bool {
        self.texture_bytes <= TEXTURE_BYTES_LIMIT
            && self.layout_cache_bytes <= LAYOUT_CACHE_BYTES_LIMIT
            && self.undo_bytes <= UNDO_BYTES_LIMIT
            && self.transaction_p95_us <= TRANSACTION_P95_US_LIMIT
            && self.render_commit_p95_us <= RENDER_COMMIT_P95_US_LIMIT
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn write_atomic(&self, path: &Path) -> std::io::Result<()> {
        let bytes = self.to_json().map_err(std::io::Error::other)?.into_bytes();
        let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(temporary, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_histogram_has_deterministic_p95_without_retaining_samples() {
        let mut histogram = FixedHistogram::default();
        for sample in [1, 2, 3, 4, 100, 101, 102, 103, 104, 105] {
            histogram.observe_us(sample);
        }
        assert_eq!(histogram.p95_us(), 105);
        assert_eq!(histogram.retained_sample_count(), 0);
        assert_eq!(histogram.bucket_count(), FIXED_HISTOGRAM_BUCKETS);
    }

    #[test]
    fn diagnostics_json_has_exact_numeric_contract_and_gate_result() {
        let diagnostics = Diagnostics::from_values(1, 2, 3, 4, 5);
        let json = diagnostics.to_json().expect("diagnostics JSON");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        let object = value.as_object().expect("JSON object");
        assert_eq!(object.len(), 5);
        for key in [
            "texture_bytes",
            "layout_cache_bytes",
            "undo_bytes",
            "transaction_p95_us",
            "render_commit_p95_us",
        ] {
            assert!(object.get(key).is_some_and(serde_json::Value::is_u64));
        }
        assert!(diagnostics.gates_pass());
    }

    #[test]
    fn slow_samples_remain_visible_and_fail_render_gate() {
        let mut histogram = FixedHistogram::default();
        histogram.observe_us(16_001);
        assert_eq!(histogram.p95_us(), 16_001);
        let diagnostics = Diagnostics::from_values(1, 2, 3, 4, histogram.p95_us());
        assert!(!diagnostics.gates_pass());
    }
}
