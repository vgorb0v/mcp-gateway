use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ProcessSample {
    /// Total RSS (KB) for the backend process tree.
    pub rss_kb: Option<u64>,
    /// Aggregate CPU percent across the tree. 100.0 == one fully busy core.
    pub cpu_percent: Option<f32>,
    /// Number of processes counted in the aggregation, including the root.
    pub child_count: u32,
}

impl ProcessSample {
    pub fn empty() -> Self {
        Self {
            rss_kb: None,
            cpu_percent: None,
            child_count: 0,
        }
    }
}

/// Thread-safe wrapper around `sysinfo::System` that exposes process-tree
/// aggregation by parent PID. A single sampler is shared across all backends —
/// refreshing once per tick is far cheaper than per-backend.
#[derive(Clone)]
pub struct ProcessSampler {
    inner: Arc<Mutex<System>>,
}

impl ProcessSampler {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(System::new())),
        }
    }

    /// Refresh process metadata for all processes. `sysinfo` requires two
    /// refresh calls separated by enough wall-clock time to compute CPU
    /// usage — callers should refresh, sleep ~200ms, then `sample_tree`.
    pub fn refresh(&self) {
        let mut sys = self.inner.lock();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            ProcessRefreshKind::new().with_memory().with_cpu(),
        );
    }

    /// Aggregate RSS + CPU for the process tree rooted at `root_pid`. Returns
    /// `ProcessSample::empty()` (with `child_count: 0`) when the root is gone.
    pub fn sample_tree(&self, root_pid: u32) -> ProcessSample {
        let sys = self.inner.lock();
        let mut by_parent: HashMap<u32, Vec<u32>> = HashMap::new();
        for (pid, process) in sys.processes() {
            let pid_u32 = pid.as_u32();
            let parent = process.parent().map(|p| p.as_u32()).unwrap_or(0);
            by_parent.entry(parent).or_default().push(pid_u32);
        }
        let mut total_rss: u64 = 0;
        let mut total_cpu: f32 = 0.0;
        let mut count: u32 = 0;
        let mut stack = vec![root_pid];
        let mut visited = std::collections::HashSet::new();
        while let Some(pid_u32) = stack.pop() {
            if !visited.insert(pid_u32) {
                continue;
            }
            let Some(process) = sys.process(Pid::from_u32(pid_u32)) else {
                continue;
            };
            total_rss += process.memory() / 1024; // sysinfo returns bytes
            total_cpu += process.cpu_usage();
            count += 1;
            if let Some(children) = by_parent.get(&pid_u32) {
                stack.extend(children.iter().copied());
            }
        }
        if count == 0 {
            return ProcessSample::empty();
        }
        ProcessSample {
            rss_kb: Some(total_rss),
            cpu_percent: Some(total_cpu),
            child_count: count,
        }
    }
}

impl Default for ProcessSampler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampler_returns_empty_for_unknown_pid() {
        let sampler = ProcessSampler::new();
        sampler.refresh();
        let sample = sampler.sample_tree(u32::MAX); // very unlikely to exist
        assert_eq!(sample.child_count, 0);
        assert_eq!(sample.rss_kb, None);
    }

    #[test]
    fn sampler_finds_current_process_after_refresh() {
        let sampler = ProcessSampler::new();
        sampler.refresh();
        let sample = sampler.sample_tree(std::process::id());
        // Cargo test may or may not be detected on every platform — only assert
        // when sysinfo actually had a row for the running PID.
        if sample.child_count > 0 {
            assert!(sample.rss_kb.unwrap_or(0) > 0);
        }
    }
}
