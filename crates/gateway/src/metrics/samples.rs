use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;

pub const DEFAULT_SAMPLE_HISTORY: usize = 60;

#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
pub struct ResourceSample {
    pub rss_kb: Option<u64>,
    pub cpu_percent: Option<f32>,
}

#[derive(Default)]
struct Inner {
    samples: VecDeque<ResourceSample>,
    capacity: usize,
}

#[derive(Clone)]
pub struct ResourceSampleRing {
    inner: Arc<Mutex<Inner>>,
}

impl ResourceSampleRing {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                samples: VecDeque::with_capacity(capacity.max(1)),
                capacity: capacity.max(1),
            })),
        }
    }

    pub fn push(&self, sample: ResourceSample) {
        let mut inner = self.inner.lock();
        if inner.samples.len() == inner.capacity {
            inner.samples.pop_front();
        }
        inner.samples.push_back(sample);
    }

    pub fn snapshot(&self) -> Vec<ResourceSample> {
        self.inner.lock().samples.iter().copied().collect()
    }
}

impl Default for ResourceSampleRing {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_SAMPLE_HISTORY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_drops_oldest_when_full() {
        let ring = ResourceSampleRing::with_capacity(3);
        for i in 0..5u32 {
            ring.push(ResourceSample {
                rss_kb: Some(i as u64),
                cpu_percent: Some(i as f32),
            });
        }
        let snap = ring.snapshot();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].rss_kb, Some(2));
        assert_eq!(snap[2].rss_kb, Some(4));
    }
}
