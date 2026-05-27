use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;

const LATENCY_RING_CAPACITY: usize = 256;

#[derive(Default)]
struct Inner {
    messages_in: u64,
    messages_out: u64,
    bytes_in: u64,
    bytes_out: u64,
    tool_calls_total: u64,
    tool_call_errors_total: u64,
    pending_requests: u64,
    last_method: Option<String>,
    last_tool: Option<String>,
    last_error: Option<String>,
    latencies_ms: VecDeque<u64>,
}

#[derive(Clone, Default)]
pub struct BackendRpcMetrics {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct RpcMetricsSnapshot {
    pub messages_in: u64,
    pub messages_out: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub tool_calls_total: u64,
    pub tool_call_errors_total: u64,
    pub pending_requests: u64,
    pub last_method: Option<String>,
    pub last_tool: Option<String>,
    pub last_error: Option<String>,
    pub latency_p50_ms: Option<u64>,
    pub latency_p95_ms: Option<u64>,
    pub latency_samples: usize,
}

impl BackendRpcMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_outbound_request(&self, method: &str, tool_name: Option<&str>, bytes: usize) {
        let mut inner = self.inner.lock();
        inner.messages_out += 1;
        inner.bytes_out += bytes as u64;
        inner.last_method = Some(method.to_string());
        if let Some(tool) = tool_name {
            inner.last_tool = Some(tool.to_string());
            inner.tool_calls_total += 1;
        }
        inner.pending_requests = inner.pending_requests.saturating_add(1);
    }

    pub fn record_outbound_notification(&self, method: &str, bytes: usize) {
        let mut inner = self.inner.lock();
        inner.messages_out += 1;
        inner.bytes_out += bytes as u64;
        inner.last_method = Some(method.to_string());
    }

    pub fn record_inbound_bytes(&self, bytes: usize) {
        let mut inner = self.inner.lock();
        inner.messages_in += 1;
        inner.bytes_in += bytes as u64;
    }

    pub fn record_response(&self, latency: Duration, is_error: bool, error_message: Option<&str>) {
        let mut inner = self.inner.lock();
        inner.pending_requests = inner.pending_requests.saturating_sub(1);
        if is_error {
            inner.tool_call_errors_total = inner.tool_call_errors_total.saturating_add(1);
        }
        if let Some(message) = error_message {
            inner.last_error = Some(message.to_string());
        }
        let millis = latency.as_millis().min(u64::MAX as u128) as u64;
        if inner.latencies_ms.len() == LATENCY_RING_CAPACITY {
            inner.latencies_ms.pop_front();
        }
        inner.latencies_ms.push_back(millis);
    }

    pub fn record_request_dropped(&self) {
        let mut inner = self.inner.lock();
        inner.pending_requests = inner.pending_requests.saturating_sub(1);
    }

    pub fn snapshot(&self) -> RpcMetricsSnapshot {
        let inner = self.inner.lock();
        let (p50, p95) = percentile_pair(&inner.latencies_ms);
        RpcMetricsSnapshot {
            messages_in: inner.messages_in,
            messages_out: inner.messages_out,
            bytes_in: inner.bytes_in,
            bytes_out: inner.bytes_out,
            tool_calls_total: inner.tool_calls_total,
            tool_call_errors_total: inner.tool_call_errors_total,
            pending_requests: inner.pending_requests,
            last_method: inner.last_method.clone(),
            last_tool: inner.last_tool.clone(),
            last_error: inner.last_error.clone(),
            latency_p50_ms: p50,
            latency_p95_ms: p95,
            latency_samples: inner.latencies_ms.len(),
        }
    }
}

fn percentile_pair(samples: &VecDeque<u64>) -> (Option<u64>, Option<u64>) {
    if samples.is_empty() {
        return (None, None);
    }
    let mut sorted: Vec<u64> = samples.iter().copied().collect();
    sorted.sort_unstable();
    (
        Some(percentile(&sorted, 0.50)),
        Some(percentile(&sorted, 0.95)),
    )
}

fn percentile(sorted: &[u64], q: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let q = q.clamp(0.0, 1.0);
    // Nearest-rank: position = ceil(q * N), 1-indexed.
    let n = sorted.len();
    let rank = (q * n as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn empty_snapshot_has_no_percentiles() {
        let metrics = BackendRpcMetrics::new();
        let snap = metrics.snapshot();
        assert_eq!(snap.latency_p50_ms, None);
        assert_eq!(snap.latency_p95_ms, None);
        assert_eq!(snap.latency_samples, 0);
    }

    #[test]
    fn nearest_rank_percentiles_match_known_values() {
        let metrics = BackendRpcMetrics::new();
        for ms in [10u64, 20, 30, 40, 50, 60, 70, 80, 90, 100] {
            metrics.record_response(Duration::from_millis(ms), false, None);
        }
        let snap = metrics.snapshot();
        assert_eq!(snap.latency_samples, 10);
        // nearest-rank p50 of 10 samples (1-indexed rank ceil(0.5*10)=5) = 50
        assert_eq!(snap.latency_p50_ms, Some(50));
        // p95 (rank ceil(0.95*10)=10) = 100
        assert_eq!(snap.latency_p95_ms, Some(100));
    }

    #[test]
    fn ring_buffer_caps_at_capacity() {
        let metrics = BackendRpcMetrics::new();
        for ms in 0..(LATENCY_RING_CAPACITY as u64 + 16) {
            metrics.record_response(Duration::from_millis(ms), false, None);
        }
        let snap = metrics.snapshot();
        assert_eq!(snap.latency_samples, LATENCY_RING_CAPACITY);
    }

    #[test]
    fn outbound_request_with_tool_name_increments_tool_counter() {
        let metrics = BackendRpcMetrics::new();
        metrics.record_outbound_request("tools/call", Some("echo"), 128);
        let snap = metrics.snapshot();
        assert_eq!(snap.messages_out, 1);
        assert_eq!(snap.bytes_out, 128);
        assert_eq!(snap.tool_calls_total, 1);
        assert_eq!(snap.last_method.as_deref(), Some("tools/call"));
        assert_eq!(snap.last_tool.as_deref(), Some("echo"));
        assert_eq!(snap.pending_requests, 1);
    }

    #[test]
    fn record_response_marks_error_and_decrements_pending() {
        let metrics = BackendRpcMetrics::new();
        metrics.record_outbound_request("tools/call", Some("echo"), 64);
        metrics.record_response(Duration::from_millis(15), true, Some("boom"));
        let snap = metrics.snapshot();
        assert_eq!(snap.tool_call_errors_total, 1);
        assert_eq!(snap.last_error.as_deref(), Some("boom"));
        assert_eq!(snap.pending_requests, 0);
    }
}
