//! Opt-in aggregate timings. Nested metrics are inclusive, not additive.
use crate::{BlockDevice, DeviceError};
use std::{
    collections::BTreeMap,
    fmt::Write,
    sync::{Arc, Mutex},
    time::Instant,
};

#[derive(Debug)]
struct Metric {
    count: u64,
    bytes: u64,
    errors: u64,
    ns: u128,
    max_ns: u64,
    buckets: [u64; 64],
}
impl Default for Metric {
    fn default() -> Self {
        Self {
            count: 0,
            bytes: 0,
            errors: 0,
            ns: 0,
            max_ns: 0,
            buckets: [0; 64],
        }
    }
}
impl Metric {
    fn add(&mut self, ns: u64, bytes: u64, failed: bool) {
        self.count += 1;
        self.bytes += bytes;
        self.errors += u64::from(failed);
        self.ns += u128::from(ns);
        self.max_ns = self.max_ns.max(ns);
        self.buckets[ns.max(1).ilog2() as usize] += 1;
    }
    fn percentile(&self, percent: u64) -> u64 {
        let needed = (self.count * percent).div_ceil(100);
        let mut count = 0;
        for (i, bucket) in self.buckets.iter().enumerate() {
            count += bucket;
            if count >= needed {
                return 1u64
                    .checked_shl(i as u32 + 1)
                    .map(|v| v - 1)
                    .unwrap_or(u64::MAX);
            }
        }
        0
    }
}

#[derive(Clone, Default, Debug)]
pub struct Profiler(Arc<Mutex<BTreeMap<&'static str, Metric>>>);

pub(crate) struct Span {
    profiler: Profiler,
    name: &'static str,
    start: Instant,
    bytes: u64,
    failed: bool,
}
impl Span {
    pub fn failed(&mut self, failed: bool) {
        self.failed = failed;
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        let ns = self.start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.profiler
            .0
            .lock()
            .unwrap()
            .entry(self.name)
            .or_default()
            .add(ns, self.bytes, self.failed);
    }
}
impl Profiler {
    pub(crate) fn span(&self, name: &'static str, bytes: u64) -> Span {
        Span {
            profiler: self.clone(),
            name,
            start: Instant::now(),
            bytes,
            failed: false,
        }
    }
    /// Call only when no spans are active, e.g. after filesystem opening.
    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
    pub fn json(&self) -> String {
        let metrics = self.0.lock().unwrap();
        let mut out = String::from("{\"schema\":1,\"timings\":{");
        for (i, (name, m)) in metrics.iter().enumerate() {
            if i != 0 {
                out.push(',');
            }
            write!(out, "\"{name}\":{{\"count\":{},\"attempted_bytes\":{},\"errors\":{},\"total_ns\":{},\"max_ns\":{},\"p50_upper_ns\":{},\"p95_upper_ns\":{}}}",
                m.count, m.bytes, m.errors, m.ns, m.max_ns, m.percentile(50), m.percentile(95)).unwrap();
        }
        out.push_str("}}\n");
        out
    }
}
pub(crate) fn span(profile: &Option<Profiler>, name: &'static str) -> Option<Span> {
    profile.as_ref().map(|p| p.span(name, 0))
}

pub struct ProfiledDevice<D> {
    device: D,
    profile: Profiler,
}
impl<D> ProfiledDevice<D> {
    pub fn new(device: D, profile: Profiler) -> Self {
        Self { device, profile }
    }
}
impl<D: BlockDevice> BlockDevice for ProfiledDevice<D> {
    fn capacity_bytes(&self) -> u64 {
        self.device.capacity_bytes()
    }
    fn read_at(&mut self, offset: u64, destination: &mut [u8]) -> Result<(), DeviceError> {
        let mut timer = self.profile.span("backend/read", destination.len() as u64);
        let result = self.device.read_at(offset, destination);
        timer.failed(result.is_err());
        result
    }
    fn write_at(&mut self, offset: u64, source: &[u8]) -> Result<(), DeviceError> {
        let mut timer = self.profile.span("backend/write", source.len() as u64);
        let result = self.device.write_at(offset, source);
        timer.failed(result.is_err());
        result
    }
    fn flush(&mut self) -> Result<(), DeviceError> {
        let mut timer = self.profile.span("backend/flush", 0);
        let result = self.device.flush();
        timer.failed(result.is_err());
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn histogram_and_counts() {
        let mut m = Metric::default();
        for n in [1, 4, 8, 100] {
            m.add(n, 4096, n == 100);
        }
        assert_eq!((m.count, m.bytes, m.errors, m.ns), (4, 16384, 1, 113));
        assert_eq!(m.percentile(50), 7);
        assert_eq!(m.percentile(95), 127);
        let p = Profiler::default();
        {
            let _timer = p.span("test", 4096);
        }
        assert!(p.json().contains("\"count\":1,\"attempted_bytes\":4096"));
        p.clear();
        assert!(!p.json().contains("test"));
    }
}
