//! Deterministic byte-granular persistence and explicit crash plans.
#![forbid(unsafe_code)]

use std::ops::Range;
use xffs_core::{AccessMode, BlockDevice, DeviceError, Operation, validate_range};

pub const DEFAULT_CAPACITY: u64 = 1024 * 1024;
pub const MAX_CAPACITY: u64 = 64 * 1024 * 1024;
pub const MAX_PENDING_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PENDING_WRITES: usize = 4096;
pub const MAX_TRACE_EVENTS: usize = 100_000;
/// Bounds both validation work and the selections retained by each trace event.
pub const MAX_PLAN_FRAGMENTS: usize = 4096;
pub type WriteId = u64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fragment {
    pub write_id: WriteId,
    /// Half-open range relative to the retained payload, not the device.
    pub range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingWrite {
    pub id: WriteId,
    pub offset: u64,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    Read {
        offset: u64,
        length: usize,
    },
    /// The event's operation ID is also this write's ID if it transfers bytes.
    Write {
        offset: u64,
        length: usize,
    },
    Flush {
        selections: Option<Vec<Fragment>>,
    },
    Crash {
        selections: Vec<Fragment>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed { transferred: usize },
    Failed { transferred: usize, reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceEvent {
    pub operation_id: u64,
    pub kind: EventKind,
    pub outcome: Outcome,
}

#[derive(Debug)]
pub struct SimDevice {
    durable: Vec<u8>,
    visible: Vec<u8>,
    pending: Vec<PendingWrite>,
    pending_bytes: usize,
    access: AccessMode,
    next_id: u64,
    trace: Vec<TraceEvent>,
    read_fault: Option<usize>,
    write_fault: Option<usize>,
    flush_fault: Option<Vec<Fragment>>,
}

fn limit(resource: &'static str, limit: usize) -> DeviceError {
    DeviceError::ResourceLimit {
        resource,
        limit: limit as u64,
    }
}
fn invalid(reason: &'static str) -> DeviceError {
    DeviceError::InvalidScenario { reason }
}

impl Default for SimDevice {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY).expect("default capacity is valid")
    }
}

impl SimDevice {
    pub fn new(capacity: u64) -> Result<Self, DeviceError> {
        Self::with_access(capacity, AccessMode::ReadWrite)
    }

    pub fn with_access(capacity: u64, access: AccessMode) -> Result<Self, DeviceError> {
        if capacity == 0 {
            return Err(invalid("capacity must be nonzero"));
        }
        if capacity > MAX_CAPACITY {
            return Err(DeviceError::ResourceLimit {
                resource: "capacity",
                limit: MAX_CAPACITY,
            });
        }
        Ok(Self {
            durable: vec![0; capacity as usize],
            visible: vec![0; capacity as usize],
            pending: vec![],
            pending_bytes: 0,
            access,
            next_id: 1,
            trace: vec![],
            read_fault: None,
            write_fault: None,
            flush_fault: None,
        })
    }

    pub fn durable_bytes(&self) -> &[u8] {
        &self.durable
    }
    pub fn visible_bytes(&self) -> &[u8] {
        &self.visible
    }
    pub fn pending_writes(&self) -> &[PendingWrite] {
        &self.pending
    }
    pub fn trace(&self) -> &[TraceEvent] {
        &self.trace
    }
    /// Does not reset IDs, pending writes, durable bytes, or armed faults.
    pub fn clear_trace(&mut self) {
        self.trace.clear();
    }

    /// Arm/replace a one-shot failure on the next valid, nonempty read.
    pub fn fail_next_read(&mut self, after_bytes: usize) {
        self.read_fault = Some(after_bytes);
    }
    /// Arm/replace a one-shot failure on the next accepted, nonempty write.
    pub fn fail_next_write(&mut self, after_bytes: usize) {
        self.write_fault = Some(after_bytes);
    }
    /// Validate now and again when consumed, as a restart/flush can retire IDs.
    pub fn fail_next_flush(&mut self, selections: Vec<Fragment>) -> Result<(), DeviceError> {
        self.validate_plan(&selections)?;
        self.flush_fault = Some(selections);
        Ok(())
    }
    pub fn clear_faults(&mut self) {
        self.read_fault = None;
        self.write_fault = None;
        self.flush_fault = None;
    }

    fn validate_plan(&self, selections: &[Fragment]) -> Result<(), DeviceError> {
        if selections.len() > MAX_PLAN_FRAGMENTS {
            return Err(limit("plan fragments", MAX_PLAN_FRAGMENTS));
        }
        for fragment in selections {
            let write = self
                .pending
                .iter()
                .find(|w| w.id == fragment.write_id)
                .ok_or_else(|| invalid("unknown or stale write ID"))?;
            if fragment.range.start > fragment.range.end || fragment.range.end > write.payload.len()
            {
                return Err(invalid("fragment range exceeds transferred write payload"));
            }
        }
        Ok(())
    }

    fn apply_plan(&mut self, selections: &[Fragment]) {
        for fragment in selections {
            let write = self
                .pending
                .iter()
                .find(|w| w.id == fragment.write_id)
                .expect("validated plan");
            let start = write.offset as usize + fragment.range.start;
            let end = write.offset as usize + fragment.range.end;
            self.durable[start..end].copy_from_slice(&write.payload[fragment.range.clone()]);
        }
    }

    fn record(
        &mut self,
        kind: EventKind,
        action: impl FnOnce(&mut Self, u64) -> Result<usize, DeviceError>,
    ) -> Result<(), DeviceError> {
        if self.trace.len() == MAX_TRACE_EVENTS {
            return Err(limit("trace events", MAX_TRACE_EVENTS));
        }
        let id = self.next_id;
        let next_id = id.checked_add(1).ok_or(DeviceError::ResourceLimit {
            resource: "operation IDs",
            limit: u64::MAX,
        })?;
        self.next_id = next_id;
        let result = action(self, id);
        let outcome = match &result {
            Ok(n) => Outcome::Completed { transferred: *n },
            Err(e) => Outcome::Failed {
                transferred: match e {
                    DeviceError::InjectedFault { transferred, .. } => *transferred,
                    _ => 0,
                },
                reason: e.to_string(),
            },
        };
        self.trace.push(TraceEvent {
            operation_id: id,
            kind,
            outcome,
        });
        result.map(|_| ())
    }

    /// Validate all fragments before mutation. Invalid plans leave all state,
    /// including the trace and fault controls, unchanged.
    pub fn crash_and_restart(&mut self, selections: &[Fragment]) -> Result<(), DeviceError> {
        self.validate_plan(selections)?;
        self.record(
            EventKind::Crash {
                selections: selections.to_vec(),
            },
            |this, _| {
                this.apply_plan(selections);
                this.visible.copy_from_slice(&this.durable);
                this.pending.clear();
                this.pending_bytes = 0;
                this.clear_faults();
                Ok(0)
            },
        )
    }
}

impl BlockDevice for SimDevice {
    fn capacity_bytes(&self) -> u64 {
        self.visible.len() as u64
    }

    fn read_at(&mut self, offset: u64, destination: &mut [u8]) -> Result<(), DeviceError> {
        self.record(
            EventKind::Read {
                offset,
                length: destination.len(),
            },
            |this, _| {
                validate_range(this.capacity_bytes(), offset, destination.len())?;
                if destination.is_empty() {
                    return Ok(0);
                }
                let fault = this.read_fault;
                let count = fault.unwrap_or(destination.len());
                if count > destination.len() {
                    return Err(invalid("read fault prefix exceeds request"));
                }
                destination[..count]
                    .copy_from_slice(&this.visible[offset as usize..offset as usize + count]);
                this.read_fault = None;
                if fault.is_some() {
                    Err(DeviceError::InjectedFault {
                        operation: Operation::Read,
                        offset: Some(offset),
                        transferred: count,
                    })
                } else {
                    Ok(count)
                }
            },
        )
    }

    fn write_at(&mut self, offset: u64, source: &[u8]) -> Result<(), DeviceError> {
        self.record(
            EventKind::Write {
                offset,
                length: source.len(),
            },
            |this, id| {
                validate_range(this.capacity_bytes(), offset, source.len())?;
                if this.access == AccessMode::ReadOnly {
                    return Err(DeviceError::ReadOnly);
                }
                if source.is_empty() {
                    return Ok(0);
                }
                let fault = this.write_fault;
                let count = fault.unwrap_or(source.len());
                if count > source.len() {
                    return Err(invalid("write fault prefix exceeds request"));
                }
                if count > 0 {
                    if this.pending.len() == MAX_PENDING_WRITES {
                        return Err(limit("pending write records", MAX_PENDING_WRITES));
                    }
                    if count > MAX_PENDING_BYTES - this.pending_bytes {
                        return Err(limit("pending payload bytes", MAX_PENDING_BYTES));
                    }
                    this.pending.push(PendingWrite {
                        id,
                        offset,
                        payload: source[..count].to_vec(),
                    });
                    this.pending_bytes += count;
                    this.visible[offset as usize..offset as usize + count]
                        .copy_from_slice(&source[..count]);
                }
                this.write_fault = None;
                if fault.is_some() {
                    Err(DeviceError::InjectedFault {
                        operation: Operation::Write,
                        offset: Some(offset),
                        transferred: count,
                    })
                } else {
                    Ok(count)
                }
            },
        )
    }

    fn flush(&mut self) -> Result<(), DeviceError> {
        self.record(
            EventKind::Flush {
                selections: self.flush_fault.clone(),
            },
            |this, _| {
                if this.access == AccessMode::ReadOnly {
                    return Ok(0);
                }
                if let Some(plan) = &this.flush_fault {
                    this.validate_plan(plan)?;
                }
                if let Some(plan) = this.flush_fault.take() {
                    this.apply_plan(&plan);
                    return Err(DeviceError::InjectedFault {
                        operation: Operation::Flush,
                        offset: None,
                        transferred: 0,
                    });
                }
                this.durable.copy_from_slice(&this.visible);
                this.pending.clear();
                this.pending_bytes = 0;
                Ok(0)
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_ids_never_wrap_or_mutate() {
        let mut device = SimDevice::new(1).unwrap();
        device.next_id = u64::MAX;
        device.fail_next_write(0);
        let before = format!("{device:?}");
        assert!(matches!(
            device.write_at(0, b"x"),
            Err(DeviceError::ResourceLimit {
                resource: "operation IDs",
                ..
            })
        ));
        assert_eq!(format!("{device:?}"), before);
    }
}
