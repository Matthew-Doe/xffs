use std::{
    cell::RefCell,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};
use xffs_core::{
    BlockDevice, DeviceError, ReadOnlyFs, ReadWriteFs, format::*, reader::OpenOptions,
};
use xffs_sim::{Fragment, SimDevice};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn fixture(revision: FormatRevision, scenario: Option<xffs_tools::Scenario>) -> Vec<u8> {
    let p = std::env::temp_dir().join(format!(
        "xffs-write-{}-{}.img",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    xffs_tools::create_image_revision(
        &p,
        16 * 1024 * 1024,
        xffs_tools::DEMO_UUID,
        None,
        scenario,
        revision,
    )
    .unwrap();
    let b = std::fs::read(&p).unwrap();
    std::fs::remove_file(p).unwrap();
    b
}
struct Backend {
    sim: SimDevice,
    fail: Option<usize>,
    mutations: usize,
    tear: bool,
}
#[derive(Clone)]
struct Shared(Rc<RefCell<Backend>>);
impl Shared {
    fn new(b: &[u8]) -> Self {
        let mut sim = SimDevice::new(b.len() as u64).unwrap();
        for (n, chunk) in b.chunks(1024 * 1024).enumerate() {
            sim.write_at((n * 1024 * 1024) as u64, chunk).unwrap();
            sim.flush().unwrap();
        }
        sim.clear_trace();
        Self(Rc::new(RefCell::new(Backend {
            sim,
            fail: None,
            mutations: 0,
            tear: false,
        })))
    }
    fn arm(&self, n: usize, tear: bool) {
        let mut b = self.0.borrow_mut();
        b.fail = Some(n);
        b.mutations = 0;
        b.tear = tear;
    }
    fn crash(&self) {
        let mut b = self.0.borrow_mut();
        b.sim.crash_and_restart(&[]).unwrap();
        b.fail = None;
        b.sim.clear_faults();
        b.sim.clear_trace();
    }
}
impl BlockDevice for Shared {
    fn capacity_bytes(&self) -> u64 {
        self.0.borrow().sim.capacity_bytes()
    }
    fn read_at(&mut self, o: u64, b: &mut [u8]) -> std::result::Result<(), DeviceError> {
        self.0.borrow_mut().sim.read_at(o, b)
    }
    fn write_at(&mut self, o: u64, b: &[u8]) -> std::result::Result<(), DeviceError> {
        let mut s = self.0.borrow_mut();
        s.mutations += 1;
        if s.fail == Some(s.mutations) {
            let n = if s.tear { b.len() / 2 } else { 0 };
            s.sim.fail_next_write(n);
        }
        s.sim.write_at(o, b)
    }
    fn flush(&mut self) -> std::result::Result<(), DeviceError> {
        let mut s = self.0.borrow_mut();
        s.mutations += 1;
        if s.fail == Some(s.mutations) {
            let fragments = if s.tear {
                s.sim
                    .pending_writes()
                    .iter()
                    .map(|w| Fragment {
                        write_id: w.id,
                        range: 0..w.payload.len() / 2,
                    })
                    .collect()
            } else {
                vec![]
            };
            s.sim.fail_next_flush(fragments)?;
        }
        s.sim.flush()
    }
}
#[test]
fn transaction_boundaries_and_faulted_writer() {
    let bytes = fixture(FormatRevision::Two, Some(xffs_tools::Scenario::Clean));
    for tear in [false, true] {
        for failure in 1..=14 {
            let d = Shared::new(&bytes);
            let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
            let id = fs.lookup(ROOT, b"ReadMe.txt").unwrap();
            d.arm(failure, tear);
            let result = fs.set_attributes(id, Some(true), Some(123), None);
            if result.is_err() {
                assert!(matches!(fs.getattr(id), Err(FsError::Faulted)));
            }
            drop(fs);
            d.crash();
            let mut recovered =
                ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
            let i = recovered.getattr(id).unwrap();
            assert_eq!(i.executable, i.atime == 123);
            if result.is_ok() {
                assert!(i.executable);
            }
            recovered
                .set_attributes(id, Some(false), Some(456), None)
                .unwrap();
            drop(recovered);
            d.crash();
            let mut ro = ReadOnlyFs::from_device(d, OpenOptions::default()).unwrap();
            assert_eq!(ro.getattr(id).unwrap().atime, 456);
        }
    }
}
#[test]
fn recovery_interruptions_and_revision_one_no_writes() {
    let bytes = fixture(
        FormatRevision::Two,
        Some(xffs_tools::Scenario::PartialCheckpoint),
    );
    for n in 1..=12 {
        let d = Shared::new(&bytes);
        d.arm(n, true);
        let _ = ReadWriteFs::from_device(d.clone(), OpenOptions::default());
        d.crash();
        let mut fs = ReadWriteFs::from_device(d, OpenOptions::default()).unwrap();
        assert!(fs.lookup(ROOT, b"Recovered.txt").is_ok());
    }
    let d = Shared::new(&fixture(FormatRevision::One, None));
    assert!(matches!(
        ReadWriteFs::from_device(d.clone(), OpenOptions::default()),
        Err(FsError::Unsupported)
    ));
    assert_eq!(d.0.borrow().mutations, 0);
}

#[test]
fn counter_exhaustion_and_invalid_recovery_do_not_mutate() {
    let mut bytes = fixture(FormatRevision::Two, None);
    for n in [1usize, 2] {
        let b = with_revision(
            JournalControl {
                sequence: u64::MAX,
                committed: false,
                count: 0,
                checksum: 0,
            }
            .encode(n as u64)
            .unwrap(),
            FormatRevision::Two,
        );
        bytes[n * BLOCK..(n + 1) * BLOCK].copy_from_slice(&b);
    }
    let d = Shared::new(&bytes);
    let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    d.arm(999, false);
    assert!(matches!(
        fs.set_attributes(ROOT, None, Some(12), None),
        Err(FsError::CounterExhausted)
    ));
    assert_eq!(d.0.borrow().mutations, 0);
    let mut bytes = fixture(FormatRevision::Two, None);
    bytes[0] = 0;
    let d = Shared::new(&bytes);
    assert!(ReadWriteFs::from_device(d.clone(), OpenOptions::default()).is_err());
    assert_eq!(d.0.borrow().mutations, 0);
    let mut bytes = fixture(FormatRevision::Two, Some(xffs_tools::Scenario::Committed));
    bytes[515 * BLOCK + 100] ^= 1;
    let d = Shared::new(&bytes);
    assert!(ReadWriteFs::from_device(d.clone(), OpenOptions::default()).is_err());
    assert_eq!(d.0.borrow().mutations, 0);
}

#[test]
fn sparse_write_zero_fill_overflow_and_cleanup() {
    let d = Shared::new(&fixture(
        FormatRevision::Two,
        Some(xffs_tools::Scenario::Clean),
    ));
    let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    let id = fs.lookup(ROOT, b"empty.txt").unwrap();
    let free = fs.statfs().unwrap().free_blocks;
    fs.write_file(id, 0, b"secret").unwrap();
    fs.truncate(id, 2).unwrap();
    fs.truncate(id, 8192).unwrap();
    assert_eq!(fs.read_file(id, 0, 8).unwrap(), b"se\0\0\0\0\0\0");
    fs.truncate(id, 1).unwrap();
    fs.write_file(id, 5, b"x").unwrap();
    assert_eq!(fs.read_file(id, 0, 6).unwrap(), b"s\0\0\0\0x");
    for n in 0..10 {
        fs.write_file(id, (1u64 << 32) + n * 8192, b"sparse")
            .unwrap();
    }
    assert!(fs.getattr(id).unwrap().extent_count > 4);
    assert_eq!(fs.read_file(id, 1u64 << 32, 8).unwrap(), b"sparse\0\0");
    assert_eq!(fs.read_file(id, 4096, 32).unwrap(), vec![0; 32]);
    fs.truncate(id, 0).unwrap();
    assert_eq!(fs.statfs().unwrap().free_blocks, free);
    fs.write_file(id, 0, &vec![0x77; 1024 * 1024]).unwrap();
    fs.truncate(id, 3).unwrap();
    assert_eq!(fs.getattr(id).unwrap().allocated, 1);
    drop(fs);
    d.crash();
    let mut fs = ReadWriteFs::from_device(d, OpenOptions::default()).unwrap();
    assert_eq!(fs.read_file(id, 0, 16).unwrap(), vec![0x77; 3]);
    fs.truncate(id, 4096).unwrap();
    assert_eq!(fs.read_file(id, 3, 4093).unwrap(), vec![0; 4093]);
}

#[test]
fn crashes_during_write_and_multitransaction_truncation() {
    let bytes = fixture(FormatRevision::Two, Some(xffs_tools::Scenario::Clean));
    for failure in 1..=24 {
        let d = Shared::new(&bytes);
        let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
        let id = fs.lookup(ROOT, b"empty.txt").unwrap();
        d.arm(failure, true);
        let result = fs.write_file(id, 8192, b"durable");
        drop(fs);
        d.crash();
        let mut fs = ReadWriteFs::from_device(d, OpenOptions::default()).unwrap();
        let got = fs.read_file(id, 8192, 7).unwrap();
        assert!(got.is_empty() || got == b"durable");
        if result.is_ok() {
            assert_eq!(got, b"durable");
        }
    }
    let d = Shared::new(&bytes);
    let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    let id = fs.lookup(ROOT, b"empty.txt").unwrap();
    fs.write_file(id, 0, &vec![9; 1024 * 1024]).unwrap();
    drop(fs);
    d.crash();
    let large = d.0.borrow().sim.durable_bytes().to_vec();
    for failure in 1..=100 {
        let d = Shared::new(&large);
        let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
        d.arm(failure, true);
        let result = fs.truncate(id, 1);
        drop(fs);
        d.crash();
        let mut fs = ReadWriteFs::from_device(d, OpenOptions::default()).unwrap();
        let i = fs.getattr(id).unwrap();
        assert!(i.size == 1 || i.size == 1024 * 1024);
        if result.is_ok() {
            assert_eq!(i.size, 1);
        }
        assert_eq!(i.cleanup_bound, 0);
        assert_eq!(i.allocated, i.size.div_ceil(4096));
    }
}

#[test]
fn partial_write_and_memory_preflight() {
    let bytes = fixture(FormatRevision::Two, Some(xffs_tools::Scenario::Clean));
    let d = Shared::new(&bytes);
    let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    let id = fs.lookup(ROOT, b"empty.txt").unwrap();
    // First block transaction uses 21 mutations; failure is in the next one.
    d.arm(25, false);
    assert_eq!(fs.write_file(id, 0, &vec![3; 8192]).unwrap(), 4096);
    assert!(matches!(fs.getattr(id), Err(FsError::Faulted)));
    drop(fs);
    d.crash();
    let mut ro = ReadOnlyFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    assert_eq!(ro.read_file(id, 0, 4096).unwrap(), vec![3; 4096]);
    let d = Shared::new(&bytes);
    assert!(matches!(
        ReadWriteFs::from_device(
            d.clone(),
            OpenOptions {
                memory_limit: 16 * 1024 * 1024
            }
        ),
        Err(FsError::ResourceLimit)
    ));
    assert_eq!(d.0.borrow().mutations, 0);
}

#[test]
fn namespace_and_open_unlinked_lifetimes() {
    let d = Shared::new(&fixture(FormatRevision::Two, None));
    let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    let initial = fs.statfs().unwrap();
    let a = fs.mkdir(ROOT, b"a").unwrap();
    let b = fs.mkdir(ROOT, b"b").unwrap();
    let f = fs.create(a, "Café".as_bytes(), false).unwrap();
    fs.write_file(f, 0, b"original").unwrap();
    assert!(matches!(
        fs.create(a, "CAFE\u{301}".as_bytes(), false),
        Err(FsError::AlreadyExists)
    ));
    assert!(matches!(
        fs.create(a, b"bad:name", false),
        Err(FsError::InvalidName)
    ));
    assert!(matches!(fs.rmdir(ROOT, b"a"), Err(FsError::NotEmpty)));
    assert!(matches!(
        fs.rename(ROOT, b"a", a, b"cycle", false),
        Err(FsError::InvalidInput)
    ));
    fs.rename(a, "Café".as_bytes(), b, b"new", false).unwrap();
    assert_eq!(fs.getattr(f).unwrap().parent, b);
    fs.rename(b, b"new", b, b"NEW", false).unwrap();
    let replaced = fs.create(b, b"old", false).unwrap();
    fs.write_file(replaced, 0, b"replaced").unwrap();
    fs.open_handle(f).unwrap();
    fs.open_handle(replaced).unwrap();
    assert!(matches!(
        fs.rename(b, b"NEW", b, b"old", true),
        Err(FsError::AlreadyExists)
    ));
    fs.rename(b, b"NEW", b, b"old", false).unwrap();
    assert_eq!(fs.lookup(b, b"old").unwrap(), f);
    assert_eq!(fs.read_file(replaced, 0, 20).unwrap(), b"replaced");
    fs.append(replaced, b"!").unwrap();
    assert_eq!(fs.read_file(replaced, 0, 20).unwrap(), b"replaced!");
    fs.close_handle(replaced).unwrap();
    assert!(fs.getattr(replaced).is_err());
    let reused = fs.create(b, b"reuse", false).unwrap();
    assert_eq!(reused.index, replaced.index);
    assert_eq!(reused.generation, replaced.generation + 1);
    assert!(matches!(fs.getattr(replaced), Err(FsError::Stale)));
    fs.unlink(b, b"old").unwrap();
    fs.write_file(f, 0, b"O").unwrap();
    assert_eq!(fs.read_file(f, 0, 20).unwrap(), b"Original");
    fs.close_handle(f).unwrap();
    fs.unlink(b, b"reuse").unwrap();
    fs.rmdir(ROOT, b"a").unwrap();
    fs.rmdir(ROOT, b"b").unwrap();
    assert_eq!(fs.statfs().unwrap(), initial);
    drop(fs);
    d.crash();
    let mut ro = ReadOnlyFs::from_device(d, OpenOptions::default()).unwrap();
    assert!(ro.read_dir(ROOT, 0, 32).unwrap().entries.is_empty());
}

#[test]
fn namespace_crashes_are_atomic_and_reclaim_orphans() {
    let d = Shared::new(&fixture(FormatRevision::Two, None));
    let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    let a = fs.mkdir(ROOT, b"a").unwrap();
    let b = fs.mkdir(ROOT, b"b").unwrap();
    let f = fs.create(a, b"source", false).unwrap();
    let g = fs.create(b, b"target", false).unwrap();
    fs.write_file(f, 0, b"new").unwrap();
    fs.write_file(g, 0, b"old").unwrap();
    drop(fs);
    d.crash();
    let bytes = d.0.borrow().sim.durable_bytes().to_vec();
    for failure in 1..=90 {
        let d = Shared::new(&bytes);
        let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
        d.arm(failure, true);
        let result = fs.rename(a, b"source", b, b"target", false);
        drop(fs);
        d.crash();
        let mut fs = ReadWriteFs::from_device(d, OpenOptions::default()).unwrap();
        let target = fs.lookup(b, b"target").unwrap();
        if target == f {
            assert!(matches!(fs.lookup(a, b"source"), Err(FsError::NotFound)));
            assert_eq!(fs.read_file(target, 0, 8).unwrap(), b"new");
            assert!(fs.getattr(g).is_err());
        } else {
            assert_eq!(target, g);
            assert_eq!(fs.lookup(a, b"source").unwrap(), f);
        }
        if result.is_ok() {
            assert_eq!(target, f);
        }
    }
}

#[test]
fn inode_exhaustion_local_directory_edits_and_full_image() {
    let d = Shared::new(&fixture(FormatRevision::Two, None));
    let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    let mut ids = vec![];
    for n in 0..255 {
        ids.push(
            fs.create(
                ROOT,
                format!("{n:03}-{}", "x".repeat(200)).as_bytes(),
                false,
            )
            .unwrap(),
        );
        d.0.borrow_mut().sim.clear_trace();
    }
    assert!(matches!(
        fs.create(ROOT, b"full", false),
        Err(FsError::NoInodes)
    ));
    assert_eq!(fs.statfs().unwrap().free_inodes, 0);
    for n in 0..255 {
        fs.unlink(ROOT, format!("{n:03}-{}", "x".repeat(200)).as_bytes())
            .unwrap();
        d.0.borrow_mut().sim.clear_trace();
    }
    let f = fs.create(ROOT, b"full", false).unwrap();
    let mut offset = 0;
    loop {
        d.0.borrow_mut().sim.clear_trace();
        match fs.write_file(f, offset, &vec![5; 1024 * 1024]) {
            Ok(n) => {
                assert!(n > 0);
                offset += n as u64;
            }
            Err(FsError::NoSpace) => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(fs.statfs().unwrap().free_blocks, 0);
    fs.rename(ROOT, b"full", ROOT, b"FULL", false).unwrap();
    fs.truncate(f, 0).unwrap();
    fs.unlink(ROOT, b"FULL").unwrap();
    drop(fs);
    d.crash();
    let fs = ReadWriteFs::from_device(d, OpenOptions::default()).unwrap();
    assert_eq!(fs.statfs().unwrap().free_inodes, 255);
    assert_eq!(fs.statfs().unwrap().free_blocks, 3561);
}
