#[path = "../../../tests/support/mod.rs"]
mod support;
use support::{TempImage, readonly_contract, writable_contract};
use xffs_core::{AccessMode, BlockDevice, DeviceError, ImageDevice, Operation};
use xffs_sim::*;

fn fragment(device: &SimDevice, index: usize, range: std::ops::Range<usize>) -> Fragment {
    Fragment {
        write_id: device.pending_writes()[index].id,
        range,
    }
}

#[test]
fn same_contract_for_both_backends() {
    let mut simulator = SimDevice::new(32).unwrap();
    writable_contract(&mut simulator);
    simulator.crash_and_restart(&[]).unwrap();
    let image = TempImage::new(32);
    let mut real = ImageDevice::open(&image.0, AccessMode::ReadWrite).unwrap();
    writable_contract(&mut real);
    let mut bytes = [0; 32];
    real.read_at(0, &mut bytes).unwrap();
    assert_eq!(simulator.durable_bytes(), bytes);
    assert_eq!(image.file().metadata().unwrap().len(), 32);
    readonly_contract(&mut SimDevice::with_access(32, AccessMode::ReadOnly).unwrap());
}

#[test]
fn lost_durable_and_repeated_restarts() {
    let mut d = SimDevice::new(8).unwrap();
    d.write_at(0, b"lost").unwrap();
    assert_eq!(&d.visible_bytes()[..4], b"lost");
    assert_eq!(d.durable_bytes(), [0; 8]);
    let old_id = d.pending_writes()[0].id;
    d.crash_and_restart(&[]).unwrap();
    assert_eq!(d.visible_bytes(), [0; 8]);
    d.write_at(0, b"safe").unwrap();
    assert!(d.pending_writes()[0].id > old_id);
    d.flush().unwrap();
    assert!(d.pending_writes().is_empty());
    d.write_at(0, b"oops").unwrap();
    d.crash_and_restart(&[]).unwrap();
    d.crash_and_restart(&[]).unwrap();
    assert_eq!(&d.visible_bytes()[..4], b"safe");
}

#[test]
fn torn_selective_and_reordered_overlapping_writes() {
    let mut d = SimDevice::new(8).unwrap();
    d.write_at(0, b"AAAA").unwrap();
    d.write_at(2, b"bbbb").unwrap();
    d.write_at(6, b"xx").unwrap();
    let plan = [fragment(&d, 1, 0..4), fragment(&d, 0, 1..4)];
    d.crash_and_restart(&plan).unwrap();
    assert_eq!(d.durable_bytes(), b"\0AAAbb\0\0");
    assert_eq!(d.visible_bytes(), d.durable_bytes());
    assert!(d.pending_writes().is_empty());
}

#[test]
fn partial_read_and_write_failures() {
    let mut d = SimDevice::new(8).unwrap();
    for prefix in [0, 2, 4] {
        d.fail_next_write(prefix);
        assert!(
            matches!(d.write_at(1, b"abcd"), Err(DeviceError::InjectedFault { operation: Operation::Write, transferred, offset: Some(1) }) if transferred == prefix)
        );
        assert_eq!(&d.visible_bytes()[1..1 + prefix], &b"abcd"[..prefix]);
        let plan = if prefix == 0 {
            assert!(d.pending_writes().is_empty());
            vec![]
        } else {
            assert_eq!(d.pending_writes()[0].payload.len(), prefix);
            vec![fragment(&d, 0, 0..prefix)]
        };
        d.crash_and_restart(&plan).unwrap();
        let mut out = [99; 4];
        d.fail_next_read(prefix);
        assert!(
            matches!(d.read_at(1, &mut out), Err(DeviceError::InjectedFault { operation: Operation::Read, transferred, .. }) if transferred == prefix)
        );
        assert_eq!(&out[..prefix], &d.visible_bytes()[1..1 + prefix]);
        assert_eq!(&out[prefix..], &vec![99; 4 - prefix]);
        d.read_at(1, &mut out).unwrap(); // one-shot consumed
    }
}

#[test]
fn failed_flush_persists_only_selection_and_keeps_pending() {
    let mut d = SimDevice::new(8).unwrap();
    d.write_at(0, b"ABCDEFGH").unwrap();
    d.flush().unwrap();
    d.write_at(0, b"1234").unwrap();
    d.write_at(2, b"xyz").unwrap();
    d.fail_next_flush(vec![fragment(&d, 1, 1..3)]).unwrap();
    assert!(matches!(
        d.flush(),
        Err(DeviceError::InjectedFault {
            operation: Operation::Flush,
            ..
        })
    ));
    assert_eq!(d.durable_bytes(), b"ABCyzFGH");
    assert_eq!(d.visible_bytes(), b"12xyzFGH");
    assert_eq!(d.pending_writes().len(), 2);
    d.crash_and_restart(&[]).unwrap();
    assert_eq!(d.visible_bytes(), b"ABCyzFGH");
    d.write_at(0, b"retry").unwrap();
    d.fail_next_flush(vec![]).unwrap();
    assert!(d.flush().is_err());
    d.flush().unwrap();
    d.crash_and_restart(&[]).unwrap();
    assert_eq!(d.visible_bytes(), b"retryFGH");
}

#[test]
fn invalid_plans_are_atomic_including_trace_and_faults() {
    let mut d = SimDevice::new(8).unwrap();
    d.write_at(0, b"old").unwrap();
    let stale = fragment(&d, 0, 0..3);
    d.flush().unwrap();
    d.fail_next_write(2);
    assert!(d.write_at(0, b"new!").is_err());
    let valid = fragment(&d, 0, 0..2);
    let invalid_plans = [
        vec![valid.clone(), stale],
        vec![
            valid.clone(),
            Fragment {
                write_id: valid.write_id,
                range: 0..3,
            },
        ],
        vec![Fragment {
            write_id: valid.write_id,
            range: std::ops::Range { start: 2, end: 1 },
        }],
        vec![valid.clone(); MAX_PLAN_FRAGMENTS + 1],
    ];
    d.fail_next_read(0);
    for plan in invalid_plans {
        let before = format!("{d:?}");
        assert!(d.crash_and_restart(&plan).is_err());
        assert_eq!(format!("{d:?}"), before);
        assert!(d.fail_next_flush(plan).is_err());
        assert_eq!(format!("{d:?}"), before);
    }
    assert!(d.read_at(0, &mut [0]).is_err());
    d.crash_and_restart(&[valid]).unwrap();
    assert_eq!(&d.visible_bytes()[..3], b"ned");
}

#[test]
fn invalid_fault_prefix_is_rejected_without_transfer_or_consumption() {
    let mut d = SimDevice::new(8).unwrap();
    d.fail_next_write(5);
    assert!(matches!(
        d.write_at(0, b"abc"),
        Err(DeviceError::InvalidScenario { .. })
    ));
    assert_eq!(d.visible_bytes(), [0; 8]);
    assert!(d.pending_writes().is_empty());
    assert!(matches!(
        d.write_at(0, b"abcdef"),
        Err(DeviceError::InjectedFault { transferred: 5, .. })
    ));
    d.fail_next_read(5);
    let mut out = [99; 4];
    assert!(matches!(
        d.read_at(0, &mut out),
        Err(DeviceError::InvalidScenario { .. })
    ));
    assert_eq!(out, [99; 4]);
    d.clear_faults();
    d.read_at(0, &mut out).unwrap();
    assert_eq!(&out, b"abcd");
}

#[test]
fn empty_and_invalid_operations_do_not_consume_faults() {
    let mut d = SimDevice::new(8).unwrap();
    d.fail_next_read(0);
    d.fail_next_write(0);
    d.read_at(8, &mut []).unwrap();
    d.write_at(8, &[]).unwrap();
    assert!(matches!(
        d.read_at(9, &mut [0]),
        Err(DeviceError::InvalidRange { .. })
    ));
    assert!(matches!(
        d.write_at(9, b"x"),
        Err(DeviceError::InvalidRange { .. })
    ));
    assert!(matches!(
        d.read_at(0, &mut [0]),
        Err(DeviceError::InjectedFault { .. })
    ));
    assert!(matches!(
        d.write_at(0, b"x"),
        Err(DeviceError::InjectedFault { .. })
    ));
    assert!(d.pending_writes().is_empty());
}

#[test]
fn resource_limits_reject_before_accepting_writes() {
    assert!(matches!(
        SimDevice::new(0),
        Err(DeviceError::InvalidScenario { .. })
    ));
    assert!(matches!(
        SimDevice::new(MAX_CAPACITY + 1),
        Err(DeviceError::ResourceLimit { .. })
    ));
    assert_eq!(SimDevice::default().capacity_bytes(), DEFAULT_CAPACITY);
    let mut d = SimDevice::new(1).unwrap();
    for _ in 0..MAX_PENDING_WRITES {
        d.write_at(0, b"a").unwrap();
    }
    assert!(matches!(
        d.write_at(0, b"b"),
        Err(DeviceError::ResourceLimit {
            resource: "pending write records",
            ..
        })
    ));
    assert_eq!(d.visible_bytes(), b"a");
    assert_eq!(d.pending_writes().len(), MAX_PENDING_WRITES);
    d.flush().unwrap();
    d.write_at(0, b"b").unwrap();
    let mut d = SimDevice::new(MAX_PENDING_BYTES as u64).unwrap();
    d.write_at(0, &vec![1; MAX_PENDING_BYTES]).unwrap();
    d.fail_next_write(1);
    assert!(matches!(
        d.write_at(0, &[2, 3]),
        Err(DeviceError::ResourceLimit {
            resource: "pending payload bytes",
            ..
        })
    ));
    assert_eq!(d.visible_bytes()[0], 1);
    assert_eq!(d.pending_writes().len(), 1);
    d.flush().unwrap();
    assert!(matches!(
        d.write_at(0, &[2, 3]),
        Err(DeviceError::InjectedFault { transferred: 1, .. })
    ));
}

#[test]
fn trace_exhaustion_blocks_mutation_and_can_be_cleared() {
    let mut d = SimDevice::new(1).unwrap();
    d.write_at(0, b"a").unwrap();
    let old_id = d.pending_writes()[0].id;
    for _ in 1..MAX_TRACE_EVENTS {
        d.read_at(0, &mut []).unwrap();
    }
    assert_eq!(d.trace().len(), MAX_TRACE_EVENTS);
    assert!(matches!(
        d.write_at(0, b"b"),
        Err(DeviceError::ResourceLimit {
            resource: "trace events",
            ..
        })
    ));
    assert!(d.flush().is_err());
    assert!(d.crash_and_restart(&[]).is_err());
    assert_eq!(d.visible_bytes(), b"a");
    assert_eq!(d.durable_bytes(), [0]);
    d.clear_trace();
    assert!(d.trace().is_empty());
    d.crash_and_restart(&[]).unwrap();
    d.write_at(0, b"c").unwrap();
    assert!(d.pending_writes()[0].id > old_id);
}

fn scenario() -> SimDevice {
    let mut d = SimDevice::new(8).unwrap();
    d.write_at(1, b"AAAA").unwrap();
    d.fail_next_write(2);
    assert!(d.write_at(2, b"bbb").is_err());
    let plan = [fragment(&d, 1, 0..2), fragment(&d, 0, 0..2)];
    d.fail_next_flush(vec![plan[0].clone()]).unwrap();
    assert!(d.flush().is_err());
    d.crash_and_restart(&plan).unwrap();
    d.read_at(0, &mut [0; 8]).unwrap();
    d.flush().unwrap();
    d
}

#[test]
fn traces_and_bytes_are_deterministic_and_describe_outcomes() {
    let a = scenario();
    let b = scenario();
    assert_eq!(a.durable_bytes(), b.durable_bytes());
    assert_eq!(a.trace(), b.trace());
    assert_eq!(
        a.trace().iter().map(|e| e.operation_id).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5, 6]
    );
    assert!(matches!(
        a.trace()[1].outcome,
        Outcome::Failed { transferred: 2, .. }
    ));
    assert!(
        matches!(&a.trace()[2].kind, EventKind::Flush { selections: Some(plan) } if plan.len() == 1)
    );
    assert!(matches!(&a.trace()[3].kind, EventKind::Crash { selections } if selections.len() == 2));
}

#[test]
fn repeated_and_empty_fragments_follow_list_order() {
    let mut d = SimDevice::new(4).unwrap();
    d.write_at(0, b"AAAA").unwrap();
    d.write_at(0, b"BBBB").unwrap();
    let a = fragment(&d, 0, 0..4);
    let b = fragment(&d, 1, 0..4);
    let empty = fragment(&d, 1, 4..4);
    d.crash_and_restart(&[a.clone(), b, a, empty]).unwrap();
    assert_eq!(d.durable_bytes(), b"AAAA");
}

#[test]
fn flush_durably_commits_partial_write_prefix() {
    let mut d = SimDevice::new(4).unwrap();
    d.fail_next_write(2);
    assert!(d.write_at(0, b"abcd").is_err());
    let retired = fragment(&d, 0, 0..2);
    d.flush().unwrap();
    assert!(d.crash_and_restart(&[retired]).is_err());
    d.write_at(0, b"xxxx").unwrap();
    d.crash_and_restart(&[]).unwrap();
    assert_eq!(d.durable_bytes(), b"ab\0\0");
}
