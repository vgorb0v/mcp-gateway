pub mod process;
pub mod rpc;
pub mod samples;

use std::time::Duration;

use crate::registry::BackendRegistry;

pub use process::{ProcessSample, ProcessSampler};
pub use rpc::{BackendRpcMetrics, RpcMetricsSnapshot};
pub use samples::{ResourceSample, ResourceSampleRing, DEFAULT_SAMPLE_HISTORY};

/// Default cadence for the metrics sampler when nothing else is configured.
pub const DEFAULT_METRICS_SAMPLE_INTERVAL_SECS: u64 = 5;

/// CPU refresh requires a short delta window so consecutive `refresh` calls
/// can compute CPU% deltas. `sysinfo` recommends >= 200ms.
const SYSINFO_CPU_WARMUP: Duration = Duration::from_millis(200);

/// Spawns a background task that periodically samples backend process trees
/// and writes the latest RSS/CPU sample back onto each backend. The task keeps
/// `/servers` and `mcpgateway ps` cheap by avoiding process-tree walks in
/// the request hot path.
pub fn spawn_sampler_task(registry: BackendRegistry, sampler: ProcessSampler, interval: Duration) {
    tokio::spawn(async move {
        let interval = if interval.is_zero() {
            Duration::from_secs(DEFAULT_METRICS_SAMPLE_INTERVAL_SECS)
        } else {
            interval
        };
        // Prime the sampler so the first emitted CPU% is meaningful.
        sampler.refresh();
        tokio::time::sleep(SYSINFO_CPU_WARMUP).await;
        loop {
            sampler.refresh();
            // Brief warmup window between two refreshes lets sysinfo compute CPU.
            tokio::time::sleep(SYSINFO_CPU_WARMUP).await;
            sampler.refresh();

            for snapshot in registry.metrics_snapshots().await {
                let proc_sample = match snapshot.pid {
                    Some(pid) => sampler.sample_tree(pid),
                    None => ProcessSample::empty(),
                };
                // Cache the freshly-collected RSS/CPU on the backend so future
                // `health()` calls return it without spawning `ps`.
                let cached = match snapshot.pid {
                    Some(_) if proc_sample.child_count > 0 => Some(ResourceSample {
                        rss_kb: proc_sample.rss_kb,
                        cpu_percent: proc_sample.cpu_percent,
                    }),
                    Some(_) => Some(ResourceSample {
                        rss_kb: snapshot.rss_kb_fallback,
                        cpu_percent: None,
                    }),
                    None => None,
                };
                registry.update_resource_sample(&snapshot.name, cached);
            }

            let sleep_for = interval.saturating_sub(SYSINFO_CPU_WARMUP);
            tokio::time::sleep(sleep_for).await;
        }
    });
}
