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
    tear_at: usize,
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
            tear_at: BLOCK / 2,
        })))
    }
    fn arm(&self, n: usize, tear: bool) {
        let mut b = self.0.borrow_mut();
        b.fail = Some(n);
        b.mutations = 0;
        b.tear = tear;
        b.tear_at = BLOCK / 2;
    }
    fn crash(&self) {
        let mut b = self.0.borrow_mut();
        let fragments = if b.tear {
            b.sim
                .pending_writes()
                .iter()
                .map(|w| Fragment {
                    write_id: w.id,
                    range: 0..w.payload.len().min(b.tear_at),
                })
                .collect()
        } else {
            vec![]
        };
        b.sim.crash_and_restart(&fragments).unwrap();
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
            let n = if s.tear { b.len().min(s.tear_at) } else { 0 };
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
                        range: 0..w.payload.len().min(s.tear_at),
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
    let fragmented = fs.create(ROOT, b"cow-space", false).unwrap();
    fs.write_file(fragmented, 0, &vec![7; 3 * BLOCK]).unwrap();
    for logical in [5, 7, 9] {
        fs.write_file(fragmented, logical * BLOCK as u64, b"x")
            .unwrap();
    }
    let fragmented_before = fs.read_file(fragmented, 0, 10 * BLOCK).unwrap();
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
    d.arm(usize::MAX, false);
    assert!(matches!(fs.write_file(f, 0, b"x"), Err(FsError::NoSpace)));
    assert_eq!(d.0.borrow().mutations, 0);
    assert_eq!(fs.read_file(f, 0, BLOCK).unwrap(), vec![5; BLOCK]);
    fs.rename(ROOT, b"full", ROOT, b"FULL", false).unwrap();
    fs.truncate(f, offset - BLOCK as u64).unwrap();
    assert_eq!(fs.statfs().unwrap().free_blocks, 1);
    // Splitting the first extent needs a data block plus a new overflow block.
    // The separate EOF tail/data update also needs two fresh blocks.
    for position in [BLOCK as u64, 11 * BLOCK as u64] {
        d.arm(usize::MAX, false);
        assert!(matches!(
            fs.write_file(fragmented, position, b"z"),
            Err(FsError::NoSpace)
        ));
        assert_eq!(d.0.borrow().mutations, 0);
        assert_eq!(
            fs.read_file(fragmented, 0, 10 * BLOCK).unwrap(),
            fragmented_before
        );
        assert_eq!(fs.statfs().unwrap().free_blocks, 1);
    }
    fs.unlink(ROOT, b"cow-space").unwrap();
    fs.truncate(f, 0).unwrap();
    fs.write_file(f, 3, b"x").unwrap();
    assert_eq!(fs.read_file(f, 0, 4).unwrap(), b"\0\0\0x");
    fs.truncate(f, 4096).unwrap();
    assert_eq!(fs.read_file(f, 4, 4092).unwrap(), vec![0; 4092]);
    fs.unlink(ROOT, b"FULL").unwrap();
    drop(fs);
    d.crash();
    let fs = ReadWriteFs::from_device(d, OpenOptions::default()).unwrap();
    assert_eq!(fs.statfs().unwrap().free_inodes, 255);
    assert_eq!(fs.statfs().unwrap().free_blocks, 3561);
}

#[test]
fn retired_slots_and_mixed_revisions() {
    let mut bytes = fixture(FormatRevision::Two, None);
    let l = Superblock::decode(&bytes[..BLOCK], 0, bytes.len() as u64)
        .unwrap()
        .layout;
    let start = l.table_start as usize * BLOCK;
    let mut table: Block = bytes[start..start + BLOCK].try_into().unwrap();
    table[320..576].copy_from_slice(
        &free_inode(InodeId {
            index: 1,
            generation: u64::MAX - 1,
        })
        .unwrap(),
    );
    bytes[start..start + BLOCK].copy_from_slice(&with_revision(table, FormatRevision::Two));
    let d = Shared::new(&bytes);
    let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    let id = fs.create(ROOT, b"last-generation", false).unwrap();
    assert_eq!(
        id,
        InodeId {
            index: 1,
            generation: u64::MAX
        }
    );
    fs.unlink(ROOT, b"last-generation").unwrap();
    assert_eq!(fs.statfs().unwrap().free_inodes, 254);
    assert_eq!(fs.create(ROOT, b"next-slot", false).unwrap().index, 2);
    drop(fs);
    d.crash();
    assert_eq!(
        ReadOnlyFs::from_device(d, OpenOptions::default())
            .unwrap()
            .statfs()
            .free_inodes,
        253
    );
    for n in [0, 1, 2, 515, l.table_start] {
        let mut bad = fixture(FormatRevision::Two, None);
        let start = n as usize * BLOCK;
        let block: Block = bad[start..start + BLOCK].try_into().unwrap();
        bad[start..start + BLOCK].copy_from_slice(&with_revision(block, FormatRevision::One));
        let d = Shared::new(&bad);
        assert!(ReadOnlyFs::from_device(d.clone(), OpenOptions::default()).is_err());
        assert!(ReadWriteFs::from_device(d.clone(), OpenOptions::default()).is_err());
        assert_eq!(d.0.borrow().mutations, 0);
    }
}

#[test]
fn every_create_remove_and_final_close_boundary() {
    let d = Shared::new(&fixture(FormatRevision::Two, None));
    let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    let id = fs.create(ROOT, b"victim", false).unwrap();
    fs.write_file(id, 0, &vec![9; 8192]).unwrap();
    fs.mkdir(ROOT, b"directory").unwrap();
    drop(fs);
    d.crash();
    let bytes = d.0.borrow().sim.durable_bytes().to_vec();
    for action in 0..5 {
        let perform = |fs: &mut ReadWriteFs<Shared>| -> Result<()> {
            match action {
                0 => {
                    fs.create(ROOT, b"created", false)?;
                }
                1 => {
                    fs.mkdir(ROOT, b"created")?;
                }
                2 => fs.unlink(ROOT, b"victim")?,
                3 => fs.rmdir(ROOT, b"directory")?,
                4 => fs.close_handle(id)?,
                _ => unreachable!(),
            }
            Ok(())
        };
        let prepare = |d: Shared| {
            let mut fs = ReadWriteFs::from_device(d, OpenOptions::default()).unwrap();
            if action == 4 {
                fs.open_handle(id).unwrap();
                fs.unlink(ROOT, b"victim").unwrap();
            }
            fs
        };
        let baseline = Shared::new(&bytes);
        let mut fs = prepare(baseline.clone());
        baseline.arm(usize::MAX, false);
        perform(&mut fs).unwrap();
        let count = baseline.0.borrow().mutations;
        assert!(count > 0);
        for tear in [false, true] {
            for failure in 1..=count + 1 {
                let d = Shared::new(&bytes);
                let mut fs = prepare(d.clone());
                d.arm(failure, tear);
                let result = perform(&mut fs);
                drop(fs);
                d.crash();
                let mut ro = ReadOnlyFs::from_device(d.clone(), OpenOptions::default()).unwrap();
                if result.is_ok() {
                    match action {
                        0 | 1 => {
                            assert!(ro.lookup(ROOT, b"created").is_ok());
                        }
                        2 | 4 => {
                            assert!(ro.lookup(ROOT, b"victim").is_err());
                        }
                        3 => {
                            assert!(ro.lookup(ROOT, b"directory").is_err());
                        }
                        _ => unreachable!(),
                    }
                }
                drop(ro);
                let mut recovered = ReadWriteFs::from_device(d, OpenOptions::default()).unwrap();
                if action == 4 {
                    assert!(recovered.getattr(id).is_err());
                }
                if let Ok(created) = recovered.lookup(ROOT, b"created") {
                    let i = recovered.getattr(created).unwrap();
                    assert_eq!(
                        i.kind,
                        if action == 0 {
                            FileKind::File
                        } else {
                            FileKind::Directory
                        }
                    );
                }
            }
        }
    }
}

#[test]
fn interrupted_opening_reclamation_and_detached_directory_validation() {
    let d = Shared::new(&fixture(FormatRevision::Two, None));
    let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    let id = fs.create(ROOT, b"orphan", false).unwrap();
    fs.write_file(id, 0, &vec![7; 80 * BLOCK]).unwrap();
    fs.open_handle(id).unwrap();
    fs.unlink(ROOT, b"orphan").unwrap();
    drop(fs);
    d.crash();
    let bytes = d.0.borrow().sim.durable_bytes().to_vec();
    let baseline = Shared::new(&bytes);
    baseline.arm(usize::MAX, false);
    let recovered = ReadWriteFs::from_device(baseline.clone(), OpenOptions::default()).unwrap();
    let count = baseline.0.borrow().mutations;
    let free = recovered.statfs().unwrap();
    for failure in 1..=count + 1 {
        let d = Shared::new(&bytes);
        d.arm(failure, true);
        let _ = ReadWriteFs::from_device(d.clone(), OpenOptions::default());
        d.crash();
        let fs = ReadWriteFs::from_device(d, OpenOptions::default()).unwrap();
        assert_eq!(fs.statfs().unwrap(), free);
    }
    // A detached empty directory may retain mappings, but never namespace entries.
    let mut bytes = fixture(FormatRevision::Two, None);
    let l = Superblock::decode(&bytes[..BLOCK], 0, bytes.len() as u64)
        .unwrap()
        .layout;
    let start = l.table_start as usize * BLOCK;
    let mut table: Block = bytes[start..start + BLOCK].try_into().unwrap();
    let mut dir = Inode::decode_revision(&table[64..320], 0, FormatRevision::Two)
        .unwrap()
        .unwrap();
    dir.id = InodeId {
        index: 1,
        generation: 1,
    };
    dir.parent = InodeId::default();
    dir.state = InodeState::Orphan;
    dir.size = 4096;
    dir.allocated = 1;
    dir.extent_count = 1;
    dir.inline = vec![Extent {
        logical: 0,
        physical: l.data_start,
        length: 1,
    }];
    table[320..576].copy_from_slice(&dir.encode_revision(FormatRevision::Two).unwrap());
    bytes[start..start + BLOCK].copy_from_slice(&with_revision(table, FormatRevision::Two));
    let mut bitmap: Block = bytes[515 * BLOCK..516 * BLOCK].try_into().unwrap();
    bitmap[64 + l.data_start as usize / 8] |= 1 << (l.data_start % 8);
    bytes[515 * BLOCK..516 * BLOCK].copy_from_slice(&with_revision(bitmap, FormatRevision::Two));
    let start = l.data_start as usize * BLOCK;
    let directory = with_revision(
        encode_block(Kind::Directory, l.data_start, dir.id, &[]).unwrap(),
        FormatRevision::Two,
    );
    bytes[start..start + BLOCK].copy_from_slice(&directory);
    let d = Shared::new(&bytes);
    let mut ro = ReadOnlyFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    assert!(ro.getattr(dir.id).is_err());
    assert_eq!(d.0.borrow().mutations, 0);
    let recovered = ReadWriteFs::from_device(d, OpenOptions::default()).unwrap();
    assert_eq!(recovered.statfs().unwrap().free_blocks, 3561);
    assert_eq!(recovered.statfs().unwrap().free_inodes, 255);
    let record = DirectoryRecord {
        name: "illegal".into(),
        child: ROOT,
    }
    .encode()
    .unwrap();
    bytes[start..start + BLOCK].copy_from_slice(&with_revision(
        encode_block(Kind::Directory, l.data_start, dir.id, &record).unwrap(),
        FormatRevision::Two,
    ));
    let d = Shared::new(&bytes);
    assert!(ReadWriteFs::from_device(d.clone(), OpenOptions::default()).is_err());
    assert_eq!(d.0.borrow().mutations, 0);
}

#[derive(Clone)]
struct SparseDevice(Rc<RefCell<SparseStorage>>);
struct SparseStorage {
    bytes: u64,
    blocks: std::collections::BTreeMap<u64, Block>,
    writes: usize,
}
impl BlockDevice for SparseDevice {
    fn capacity_bytes(&self) -> u64 {
        self.0.borrow().bytes
    }
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> std::result::Result<(), DeviceError> {
        xffs_core::validate_range(self.capacity_bytes(), offset, out.len())?;
        let storage = self.0.borrow();
        for (n, v) in out.iter_mut().enumerate() {
            let pos = offset + n as u64;
            *v = storage
                .blocks
                .get(&(pos / 4096))
                .map_or(0, |b| b[(pos % 4096) as usize]);
        }
        Ok(())
    }
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> std::result::Result<(), DeviceError> {
        xffs_core::validate_range(self.capacity_bytes(), offset, bytes.len())?;
        let mut storage = self.0.borrow_mut();
        storage.writes += 1;
        for (n, v) in bytes.iter().enumerate() {
            let pos = offset + n as u64;
            storage.blocks.entry(pos / 4096).or_insert([0; BLOCK])[(pos % 4096) as usize] = *v;
        }
        Ok(())
    }
    fn flush(&mut self) -> std::result::Result<(), DeviceError> {
        self.0.borrow_mut().writes += 1;
        Ok(())
    }
}

#[test]
fn oversized_indivisible_mapping_edit_is_rejected_before_io() {
    mapping_limit_preflight(43000);
}

#[test]
fn extent_limit_preflight_keeps_writer_usable() {
    mapping_limit_preflight(MAX_EXTENTS);
}

fn mapping_limit_preflight(count: usize) {
    // Sparse backing stores only metadata for a valid 512 MiB, 43,000-extent file.
    // Inserting its first mapping shifts >256 packed overflow blocks, while
    // appending touches only the tail. The first operation must not mutate data.
    let layout = VolumeLayout::new(512 * 1024 * 1024, Some(256)).unwrap();
    let chain_count = (count - 4).div_ceil(EXTENTS_PER_BLOCK);
    let root_block = layout.data_start;
    let data_start = root_block + 1;
    let chain_start = data_start + count as u64;
    let allocated_end = chain_start + chain_count as u64;
    let extents: Vec<_> = (0..count)
        .map(|n| Extent {
            logical: 2 * (n as u64 + 1),
            physical: data_start + n as u64,
            length: 1,
        })
        .collect();
    let mut root = Inode::decode(
        &include_bytes!("../../xffs-core/tests/golden/root-table.bin")[64..320],
        0,
    )
    .unwrap()
    .unwrap();
    root.size = 4096;
    root.allocated = 1;
    root.extent_count = 1;
    root.inline = vec![Extent {
        logical: 0,
        physical: root_block,
        length: 1,
    }];
    let mut file = root.clone();
    file.id = InodeId {
        index: 1,
        generation: 1,
    };
    file.kind = FileKind::File;
    file.size = (2 * count as u64 + 1) * 4096;
    file.allocated = count as u64 + chain_count as u64;
    file.extent_count = count;
    file.inline = extents[..4].to_vec();
    file.overflow = chain_start;
    let mut blocks = std::collections::BTreeMap::new();
    let sb = Superblock {
        revision: FormatRevision::Two,
        uuid: [0; 16],
        layout: layout.clone(),
    };
    for n in [0, layout.blocks - 1] {
        blocks.insert(n, sb.encode(n).unwrap());
    }
    for n in [1, 2] {
        blocks.insert(
            n,
            with_revision(
                JournalControl {
                    sequence: 1,
                    committed: false,
                    count: 0,
                    checksum: 0,
                }
                .encode(n)
                .unwrap(),
                FormatRevision::Two,
            ),
        );
    }
    for n in 0..layout.bitmap_blocks {
        let mut p = [0; PAYLOAD];
        for bit in 0..PAYLOAD * 8 {
            let absolute = n * (PAYLOAD * 8) as u64 + bit as u64;
            if absolute < allocated_end || absolute == layout.blocks - 1 {
                p[bit / 8] |= 1 << (bit % 8);
            }
        }
        blocks.insert(
            515 + n,
            with_revision(
                encode_block(Kind::Bitmap, 515 + n, InodeId::default(), &p).unwrap(),
                FormatRevision::Two,
            ),
        );
    }
    for n in 0..layout.table_blocks {
        let mut p = [0; 3840];
        if n == 0 {
            p[..256].copy_from_slice(&root.encode_revision(FormatRevision::Two).unwrap());
            p[256..512].copy_from_slice(&file.encode_revision(FormatRevision::Two).unwrap());
        }
        blocks.insert(
            layout.table_start + n,
            with_revision(
                encode_block(Kind::Inodes, layout.table_start + n, InodeId::default(), &p).unwrap(),
                FormatRevision::Two,
            ),
        );
    }
    for (n, chunk) in extents[4..].chunks(EXTENTS_PER_BLOCK).enumerate() {
        let physical = chain_start + n as u64;
        let next = if n + 1 == chain_count {
            0
        } else {
            physical + 1
        };
        blocks.insert(
            physical,
            with_revision(
                encode_extents(physical, file.id, next, chunk).unwrap(),
                FormatRevision::Two,
            ),
        );
    }
    let entry = DirectoryRecord {
        name: "fragmented".into(),
        child: file.id,
    }
    .encode()
    .unwrap();
    blocks.insert(
        root_block,
        with_revision(
            encode_block(Kind::Directory, root_block, ROOT, &entry).unwrap(),
            FormatRevision::Two,
        ),
    );
    let d = SparseDevice(Rc::new(RefCell::new(SparseStorage {
        bytes: layout.blocks * 4096,
        blocks,
        writes: 0,
    })));
    let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    let before = fs.statfs().unwrap();
    d.0.borrow_mut().writes = 0;
    assert!(matches!(
        fs.write_file(file.id, 0, b"x"),
        Err(FsError::TooBig)
    ));
    assert_eq!(d.0.borrow().writes, 0);
    assert_eq!(fs.statfs().unwrap(), before);
    assert_eq!(fs.getattr(file.id).unwrap(), file);
    if count == MAX_EXTENTS {
        assert!(matches!(fs.append(file.id, b"x"), Err(FsError::TooBig)));
        assert_eq!(d.0.borrow().writes, 0);
        // Replacing an existing last mapping retains the extent count.
        assert_eq!(
            fs.write_file(file.id, file.size - BLOCK as u64, b"x")
                .unwrap(),
            1
        );
        drop(fs);
        let mut ro = ReadOnlyFs::from_device(d, OpenOptions::default()).unwrap();
        assert_eq!(
            ro.read_file(file.id, file.size - BLOCK as u64, 1).unwrap(),
            b"x"
        );
        return;
    }
    assert_eq!(fs.append(file.id, b"x").unwrap(), 1);
    drop(fs);
    let mut limited = ReadWriteFs::from_device(
        d.clone(),
        OpenOptions {
            memory_limit: 24 * 1024 * 1024,
        },
    )
    .unwrap();
    d.0.borrow_mut().writes = 0;
    assert!(matches!(
        limited.write_file(file.id, 0, b"x"),
        Err(FsError::ResourceLimit)
    ));
    assert_eq!(d.0.borrow().writes, 0);
    drop(limited);
    let mut ro = ReadOnlyFs::from_device(d, OpenOptions::default()).unwrap();
    assert_eq!(ro.read_file(file.id, file.size, 1).unwrap(), b"x");
}

#[test]
fn torn_header_sequence_state_and_checksum_at_every_transaction_phase() {
    let bytes = fixture(FormatRevision::Two, Some(xffs_tools::Scenario::Clean));
    for prefix in [1, 10, 40, 64, 72, 76, 80, 88, BLOCK - 1] {
        for failure in 1..=14 {
            let d = Shared::new(&bytes);
            let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
            let id = fs.lookup(ROOT, b"ReadMe.txt").unwrap();
            d.arm(failure, true);
            d.0.borrow_mut().tear_at = prefix;
            let result = fs.set_attributes(id, Some(true), Some(123), None);
            assert!(result.is_err());
            assert!(matches!(fs.sync(), Err(FsError::Faulted)));
            drop(fs);
            d.crash();
            let mut ro = ReadOnlyFs::from_device(d.clone(), OpenOptions::default()).unwrap();
            let i = ro.getattr(id).unwrap();
            assert_eq!(i.executable, i.atime == 123);
            drop(ro);
            let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
            fs.set_attributes(id, Some(false), Some(456), None).unwrap();
            drop(fs);
            d.crash();
            let mut ro = ReadOnlyFs::from_device(d, OpenOptions::default()).unwrap();
            assert_eq!(ro.getattr(id).unwrap().atime, 456);
        }
    }
}

#[test]
fn interrupted_cow_overwrite_preserves_old_data() {
    let d = Shared::new(&fixture(FormatRevision::Two, None));
    let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    let id = fs.create(ROOT, b"in-place", false).unwrap();
    fs.write_file(id, 0, &[1; BLOCK]).unwrap();
    d.arm(1, true);
    d.0.borrow_mut().tear_at = 128;
    assert!(fs.write_file(id, 0, &[2; BLOCK]).is_err());
    drop(fs);
    d.crash();
    let mut ro = ReadOnlyFs::from_device(d, OpenOptions::default()).unwrap();
    assert_eq!(ro.getattr(id).unwrap().size, 4096);
    let bytes = ro.read_file(id, 0, BLOCK).unwrap();
    assert_eq!(bytes, [1; BLOCK]);
}

// Fault positions come from the successful operation's write/flush trace, so
// allocation and extent-chain changes cannot silently escape the crash matrix.
fn mutation_count(d: &Shared) -> usize {
    d.0.borrow()
        .sim
        .trace()
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                xffs_sim::EventKind::Write { .. } | xffs_sim::EventKind::Flush { .. }
            )
        })
        .count()
}

#[test]
fn cow_trace_boundaries_prefixes_and_reclamation() {
    let blank = fixture(FormatRevision::Two, None);
    for scenario in 0..6 {
        let setup = Shared::new(&blank);
        let mut fs = ReadWriteFs::from_device(setup.clone(), OpenOptions::default()).unwrap();
        let empty_stats = fs.statfs().unwrap();
        let id = fs.create(ROOT, b"cow", false).unwrap();
        fs.write_file(id, 0, &vec![1; 3 * BLOCK]).unwrap();
        if scenario >= 3 {
            fs.truncate(id, 17).unwrap();
        }
        let old = fs.read_file(id, 0, 4 * BLOCK).unwrap();
        let baseline = setup.0.borrow().sim.durable_bytes().to_vec();
        let (offset, data) = match scenario {
            0 => (0, vec![2; BLOCK]),
            1 => (100, vec![2; 200]),
            2 => (3000, vec![2; 6000]),
            3 => (30, vec![2; 40]),
            4 => (2 * BLOCK as u64 + 10, vec![2; 40]),
            _ => (0, vec![]),
        };
        let mut states = vec![(0, old.clone())];
        let mut expected = old;
        let mut completed = 0;
        if scenario == 5 {
            expected.resize(3 * BLOCK, 0);
            states.push((0, expected));
        } else {
            while completed < data.len() {
                let pos = offset as usize + completed;
                let count = (data.len() - completed).min(BLOCK - pos % BLOCK);
                expected.resize(expected.len().max(pos + count), 0);
                expected[pos..pos + count].copy_from_slice(&data[completed..completed + count]);
                completed += count;
                states.push((completed, expected.clone()));
            }
        }
        setup.0.borrow_mut().sim.clear_trace();
        if scenario == 5 {
            fs.truncate(id, 3 * BLOCK as u64).unwrap();
        } else {
            assert_eq!(fs.write_file(id, offset, &data).unwrap(), data.len());
        }
        let boundaries = mutation_count(&setup);
        assert!(boundaries > 0);
        drop(fs);
        for prefix in [0, 1, 80, BLOCK / 2, BLOCK - 1, BLOCK] {
            // Include a no-failure run to require successful writes to survive.
            for failure in 1..=boundaries + 1 {
                let d = Shared::new(&baseline);
                let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
                d.arm(failure, prefix != 0);
                d.0.borrow_mut().tear_at = prefix;
                let result = if scenario == 5 {
                    fs.truncate(id, 3 * BLOCK as u64).map(|()| 0)
                } else {
                    fs.write_file(id, offset, &data)
                };
                drop(fs);
                d.crash();
                let mut recovered =
                    ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
                let got = recovered.read_file(id, 0, 4 * BLOCK).unwrap();
                let state = states
                    .iter()
                    .position(|(_, bytes)| *bytes == got)
                    .unwrap_or_else(|| {
                        panic!("torn data: scenario {scenario}, failure {failure}, prefix {prefix}")
                    });
                if let Ok(n) = result {
                    assert!(states[state].0 >= n);
                    if scenario == 5 {
                        assert_eq!(state, 1);
                    }
                }
                let stats = recovered.statfs().unwrap();
                drop(recovered);
                d.crash();
                let mut recovered =
                    ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
                assert_eq!(recovered.read_file(id, 0, 4 * BLOCK).unwrap(), got);
                assert_eq!(recovered.statfs().unwrap(), stats);
                recovered.write_file(id, 0, b"reused").unwrap();
                recovered.unlink(ROOT, b"cow").unwrap();
                assert_eq!(recovered.statfs().unwrap(), empty_stats);
            }
        }
    }
}

#[test]
fn cow_recovery_can_itself_be_interrupted() {
    let d = Shared::new(&fixture(FormatRevision::Two, None));
    let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
    let id = fs.create(ROOT, b"cow", false).unwrap();
    fs.write_file(id, 0, &[1; BLOCK]).unwrap();
    let baseline = d.0.borrow().sim.durable_bytes().to_vec();
    d.0.borrow_mut().sim.clear_trace();
    fs.write_file(id, 0, &[2; BLOCK]).unwrap();
    let boundaries = mutation_count(&d);
    drop(fs);
    // Select a crash image with a committed journal awaiting checkpoint/retirement.
    let mut committed = None;
    for failure in 1..=boundaries {
        let d = Shared::new(&baseline);
        let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
        d.arm(failure, false);
        let _ = fs.write_file(id, 0, &[2; BLOCK]);
        drop(fs);
        d.crash();
        let bytes = d.0.borrow().sim.durable_bytes().to_vec();
        let a: Block = bytes[BLOCK..2 * BLOCK].try_into().unwrap();
        let b: Block = bytes[2 * BLOCK..3 * BLOCK].try_into().unwrap();
        if select_control(JournalControl::decode(&a, 1), JournalControl::decode(&b, 2))
            .unwrap()
            .0
            .committed
        {
            committed = Some(bytes);
            break;
        }
    }
    let bytes = committed.unwrap();
    let success = Shared::new(&bytes);
    let fs = ReadWriteFs::from_device(success.clone(), OpenOptions::default()).unwrap();
    drop(fs);
    for failure in 1..=mutation_count(&success) + 1 {
        for prefix in [0, 80, BLOCK / 2, BLOCK] {
            let d = Shared::new(&bytes);
            d.arm(failure, prefix != 0);
            d.0.borrow_mut().tear_at = prefix;
            let _ = ReadWriteFs::from_device(d.clone(), OpenOptions::default());
            d.crash();
            let mut fs = ReadWriteFs::from_device(d.clone(), OpenOptions::default()).unwrap();
            assert_eq!(fs.read_file(id, 0, BLOCK).unwrap(), [2; BLOCK]);
            fs.write_file(id, 0, &[3; BLOCK]).unwrap();
        }
    }
}
