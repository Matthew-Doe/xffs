use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use xffs_core::{
    BlockDevice, DeviceError, Operation, ReadOnlyFs,
    format::*,
    reader::{MAX_READ, OpenOptions},
};
use xffs_tools::{DEMO_UUID, Scenario, create_image};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Spy {
    bytes: Arc<Vec<u8>>,
    fail: Option<u64>,
}
impl BlockDevice for Spy {
    fn capacity_bytes(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn read_at(&mut self, o: u64, out: &mut [u8]) -> std::result::Result<(), DeviceError> {
        xffs_core::validate_range(self.capacity_bytes(), o, out.len())?;
        if self
            .fail
            .is_some_and(|p| o <= p && p < o + out.len() as u64)
        {
            return Err(DeviceError::InjectedFault {
                operation: Operation::Read,
                offset: Some(o),
                transferred: 0,
            });
        }
        out.copy_from_slice(&self.bytes[o as usize..o as usize + out.len()]);
        Ok(())
    }
    fn write_at(&mut self, _: u64, _: &[u8]) -> std::result::Result<(), DeviceError> {
        panic!("reader wrote storage")
    }
    fn flush(&mut self) -> std::result::Result<(), DeviceError> {
        panic!("reader flushed storage")
    }
}
fn fixture(s: Scenario) -> Vec<u8> {
    let p: PathBuf = std::env::temp_dir().join(format!(
        "xffs-recovery-{}-{}.img",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    create_image(&p, 16 * 1024 * 1024, DEMO_UUID, None, Some(s)).unwrap();
    let b = std::fs::read(&p).unwrap();
    std::fs::remove_file(p).unwrap();
    b
}
fn open(b: Vec<u8>) -> Result<ReadOnlyFs<Spy>> {
    ReadOnlyFs::from_device(
        Spy {
            bytes: Arc::new(b),
            fail: None,
        },
        OpenOptions::default(),
    )
}
fn block(b: &[u8], n: u64) -> Block {
    b[n as usize * BLOCK..(n as usize + 1) * BLOCK]
        .try_into()
        .unwrap()
}
fn put(b: &mut [u8], n: u64, v: Block) {
    b[n as usize * BLOCK..(n as usize + 1) * BLOCK].copy_from_slice(&v);
}
fn layout(b: &[u8]) -> VolumeLayout {
    Superblock::decode(&b[..BLOCK], 0, b.len() as u64)
        .unwrap()
        .layout
}
fn inode(b: &[u8], index: u64) -> Inode {
    let l = layout(b);
    let v = block(b, l.table_start + index / 15);
    let o = 64 + (index % 15) as usize * 256;
    Inode::decode(&v[o..o + 256], index).unwrap().unwrap()
}
fn set_inode(b: &mut [u8], i: Inode) {
    let l = layout(b);
    let n = l.table_start + i.id.index / 15;
    let v = block(b, n);
    let mut p = v[64..3904].to_vec();
    let o = (i.id.index % 15) as usize * 256;
    p[o..o + 256].copy_from_slice(&i.encode().unwrap());
    put(
        b,
        n,
        encode_block(Kind::Inodes, n, InodeId::default(), &p).unwrap(),
    );
}
fn directory(b: &mut [u8], id: u64, change: impl FnOnce(&mut Vec<DirectoryRecord>)) {
    let i = inode(b, id);
    let n = i.inline[0].physical;
    let mut entries = decode_directory(&block(b, n), n, i.id).unwrap();
    change(&mut entries);
    let p: Vec<u8> = entries
        .into_iter()
        .flat_map(|e| e.encode().unwrap())
        .collect();
    put(b, n, encode_block(Kind::Directory, n, i.id, &p).unwrap());
}
fn refresh_transaction(b: &mut [u8]) {
    let mut crc = 0;
    for ordinal in 0..2 {
        let n = 3 + 2 * ordinal;
        let mut d = JournalDescriptor::decode(&block(b, n), ordinal as u32).unwrap();
        d.image_checksum = crc32c::crc32c(&block(b, n + 1));
        let raw = d.encode().unwrap();
        put(b, n, raw);
        crc = crc32c::crc32c_append(crc, &raw);
        crc = crc32c::crc32c_append(crc, &block(b, n + 1));
    }
    let c = JournalControl {
        sequence: 2,
        committed: true,
        count: 2,
        checksum: crc,
    };
    for n in [1, 2] {
        put(b, n, c.encode(n).unwrap());
    }
}
#[test]
fn recovered_views_match_independent_clean_fixture_without_writes() {
    let clean = fixture(Scenario::Clean);
    let mut expected = open(clean).unwrap();
    for scenario in [
        Scenario::Clean,
        Scenario::Committed,
        Scenario::PartialCheckpoint,
    ] {
        let bytes = Arc::new(fixture(scenario));
        let before = crc32c::crc32c(&bytes);
        for _ in 0..2 {
            let mut fs = ReadOnlyFs::from_device(
                Spy {
                    bytes: bytes.clone(),
                    fail: None,
                },
                OpenOptions::default(),
            )
            .unwrap();
            assert_eq!(fs.statfs(), expected.statfs());
            assert_eq!(fs.getattr(ROOT).unwrap(), expected.getattr(ROOT).unwrap());
            let mut pending = vec![ROOT];
            while let Some(id) = pending.pop() {
                let i = fs.getattr(id).unwrap();
                assert_eq!(i, expected.getattr(id).unwrap());
                if i.kind == FileKind::Directory {
                    let all = fs.read_dir(id, 0, 1024).unwrap();
                    assert_eq!(all, expected.read_dir(id, 0, 1024).unwrap());
                    let mut got = vec![];
                    let mut cookie = 0;
                    loop {
                        let page = fs.read_dir(id, cookie, 3).unwrap();
                        got.extend(page.entries);
                        cookie = page.next_cookie;
                        if page.eof {
                            break;
                        }
                    }
                    assert_eq!(got, all.entries);
                    pending.extend(got.iter().map(|r| r.child));
                } else {
                    for offset in [0, 1, 4090, 8192, 1 << 32, (1 << 32) + 4090, u64::MAX] {
                        assert_eq!(
                            fs.read_file(id, offset, 8192).unwrap(),
                            expected.read_file(id, offset, 8192).unwrap()
                        );
                    }
                }
            }
            assert!(fs.lookup(ROOT, b"Before.txt").is_err());
            assert!(fs.lookup(ROOT, b"Recovered.txt").is_ok());
            assert_eq!(
                fs.lookup(ROOT, "CAFE\u{301}.TXT".as_bytes()).unwrap(),
                fs.lookup(ROOT, "Café.txt".as_bytes()).unwrap()
            );
            let sparse = fs.lookup(ROOT, b"sparse.bin").unwrap();
            assert_eq!(
                fs.read_file(sparse, (1 << 32) - 4, 8).unwrap(),
                [0, 0, 0, 0, 0x77, 0x77, 0x77, 0x77]
            );
            assert_eq!(fs.read_file(sparse, 4096, 4096).unwrap(), vec![0; 4096]);
            assert!(fs.read_file(sparse, 0, MAX_READ + 1).is_err());
            let many = fs.lookup(ROOT, b"many").unwrap();
            assert_eq!(fs.read_dir(many, 0, 1024).unwrap().entries.len(), 160);
            assert!(
                fs.getattr(InodeId {
                    index: 0,
                    generation: 2
                })
                .is_err()
            );
        }
        assert_eq!(crc32c::crc32c(&bytes), before);
    }
}
#[test]
fn superblock_controls_and_retirement() {
    let original = fixture(Scenario::Committed);
    let mut b = original.clone();
    b[0] ^= 1;
    assert!(
        open(b)
            .unwrap()
            .diagnostics()
            .iter()
            .any(|s| s.contains("superblock"))
    );
    let mut b = original.clone();
    let n = b.len() as u64 / 4096 - 1;
    let mut s = Superblock::decode(&block(&b, n), n, b.len() as u64).unwrap();
    s.uuid[0] ^= 1;
    put(&mut b, n, s.encode(n).unwrap());
    assert!(open(b).is_err());
    let mut b = original.clone();
    b[4096] ^= 1;
    assert_eq!(open(b).unwrap().recovered_blocks(), 2);
    let mut b = original.clone();
    b[4096] ^= 1;
    b[8192] ^= 1;
    assert!(open(b).is_err());
    // Checkpoint independently by copying every descriptor's image home, then
    // interrupt retirement after one valid clean control.
    let mut b = original.clone();
    for n in 0..2 {
        let d = JournalDescriptor::decode(&block(&b, 3 + 2 * n), n as u32).unwrap();
        let image = block(&b, 4 + 2 * n);
        put(&mut b, d.target, image);
    }
    put(
        &mut b,
        1,
        JournalControl {
            sequence: 3,
            committed: false,
            count: 0,
            checksum: 0,
        }
        .encode(1)
        .unwrap(),
    );
    let mut fs = open(b.clone()).unwrap();
    assert_eq!(fs.recovered_blocks(), 0);
    assert!(fs.lookup(ROOT, b"Recovered.txt").is_ok());
    b[4096] ^= 1;
    assert_eq!(open(b).unwrap().recovered_blocks(), 2);
    // Counter maximum is readable, never incremented by recovery.
    let mut b = fixture(Scenario::Clean);
    for n in [1, 2] {
        put(
            &mut b,
            n,
            JournalControl {
                sequence: u64::MAX,
                committed: false,
                count: 0,
                checksum: 0,
            }
            .encode(n)
            .unwrap(),
        );
    }
    assert!(open(b).is_ok());
}
#[test]
fn rejects_invalid_payloads_and_targets() {
    let original = fixture(Scenario::Committed);
    let mut b = original.clone();
    b[4 * BLOCK + 100] ^= 1;
    assert!(open(b).is_err());
    let mut b = original.clone();
    let mut c = JournalControl::decode(&block(&b, 1), 1).unwrap();
    c.checksum ^= 1;
    for n in [1, 2] {
        put(&mut b, n, c.encode(n).unwrap());
    }
    assert!(open(b).is_err());
    let mut b = original.clone();
    let image = block(&b, 4);
    put(&mut b, 6, image);
    let mut d = JournalDescriptor::decode(&block(&b, 5), 1).unwrap();
    d.target = JournalDescriptor::decode(&block(&b, 3), 0).unwrap().target;
    put(&mut b, 5, d.encode().unwrap());
    refresh_transaction(&mut b);
    assert!(open(b).is_err());
    let mut b = original.clone();
    let image = block(&b, 0);
    put(&mut b, 4, image);
    let mut d = JournalDescriptor::decode(&block(&b, 3), 0).unwrap();
    d.target = 0;
    put(&mut b, 3, d.encode().unwrap());
    refresh_transaction(&mut b);
    assert!(open(b).is_err());
    // A syntactically valid directory image may not replace file data.
    let mut b = original;
    let file = inode(&b, 3);
    let target = file.inline[0].physical;
    put(
        &mut b,
        4,
        encode_block(Kind::Directory, target, file.id, &[]).unwrap(),
    );
    let mut d = JournalDescriptor::decode(&block(&b, 3), 0).unwrap();
    d.target = target;
    put(&mut b, 3, d.encode().unwrap());
    refresh_transaction(&mut b);
    assert!(open(b).is_err());
}
#[test]
fn rejects_corrupt_ownership_namespace_and_resource_exhaustion() {
    let original = fixture(Scenario::Clean);
    let mut b = original.clone();
    let a = inode(&b, 3);
    let mut other = inode(&b, 4);
    other.inline = a.inline.clone();
    other.size = a.size;
    other.allocated = a.allocated;
    set_inode(&mut b, other);
    assert!(open(b).is_err());
    let mut b = original.clone();
    directory(&mut b, 0, |e| e[1].child.generation += 1);
    assert!(open(b).is_err());
    let mut b = original.clone();
    directory(&mut b, 0, |e| e[2].name = "README.TXT".into());
    assert!(open(b).is_err());
    let mut b = original.clone();
    directory(&mut b, 0, |e| {
        e.remove(0);
    });
    assert!(open(b).is_err());
    let mut b = original.clone();
    directory(&mut b, 0, |e| e[0].child = ROOT);
    assert!(open(b).is_err());
    let mut b = original.clone();
    let l = layout(&b);
    let v = block(&b, 515);
    let mut p = v[64..].to_vec();
    p[(l.data_start as usize + 300) / 8] |= 1 << ((l.data_start + 300) % 8);
    put(
        &mut b,
        515,
        encode_block(Kind::Bitmap, 515, InodeId::default(), &p).unwrap(),
    );
    assert!(open(b).is_err());
    assert!(matches!(
        ReadOnlyFs::from_device(
            Spy {
                bytes: Arc::new(original),
                fail: None
            },
            OpenOptions { memory_limit: 1024 }
        ),
        Err(FsError::ResourceLimit)
    ));
}
#[test]
fn detached_owners_and_mapped_read_errors() {
    for state in [InodeState::Orphan, InodeState::Pending] {
        let mut b = fixture(Scenario::Clean);
        let mut i = inode(&b, 3);
        i.state = state;
        i.parent = InodeId::default();
        set_inode(&mut b, i);
        directory(&mut b, 0, |e| e.retain(|r| r.name != "ReadMe.txt"));
        let mut fs = open(b).unwrap();
        assert!(fs.lookup(ROOT, b"ReadMe.txt").is_err());
        assert!(fs.inode_id(3).is_err());
    }
    let b = fixture(Scenario::Clean);
    let i = inode(&b, 3);
    let mut fs = ReadOnlyFs::from_device(
        Spy {
            bytes: Arc::new(b),
            fail: Some(i.inline[0].physical * 4096),
        },
        OpenOptions::default(),
    )
    .unwrap();
    assert!(matches!(fs.read_file(i.id, 0, 16), Err(FsError::Device(_))));
}
#[test]
fn bitmap_and_overflow_metadata_are_read_through_overlay() {
    for target in [515, inode(&fixture(Scenario::Committed), 7).overflow] {
        assert_ne!(target, 0);
        let mut b = fixture(Scenario::Committed);
        let image = block(&b, target);
        put(&mut b, 6, image);
        let mut d = JournalDescriptor::decode(&block(&b, 5), 1).unwrap();
        d.target = target;
        put(&mut b, 5, d.encode().unwrap());
        refresh_transaction(&mut b);
        b[target as usize * BLOCK] ^= 1; // home is unreadable; the valid image is authoritative
        let mut fs = open(b).unwrap();
        assert!(fs.lookup(ROOT, b"Recovered.txt").is_ok());
        let id = fs.lookup(ROOT, b"overflow.bin").unwrap();
        assert_eq!(fs.read_file(id, 4 * 4096, 4096).unwrap(), vec![0x5a; 4096]);
    }
}
#[test]
fn metadata_is_rechecked_after_open_and_trailing_bytes_are_ignored() {
    struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl BlockDevice for Shared {
        fn capacity_bytes(&self) -> u64 {
            self.0.lock().unwrap().len() as u64
        }
        fn read_at(&mut self, o: u64, out: &mut [u8]) -> std::result::Result<(), DeviceError> {
            let b = self.0.lock().unwrap();
            xffs_core::validate_range(b.len() as u64, o, out.len())?;
            out.copy_from_slice(&b[o as usize..o as usize + out.len()]);
            Ok(())
        }
        fn write_at(&mut self, _: u64, _: &[u8]) -> std::result::Result<(), DeviceError> {
            panic!("write")
        }
        fn flush(&mut self) -> std::result::Result<(), DeviceError> {
            panic!("flush")
        }
    }
    let mut original = fixture(Scenario::Clean);
    original.extend([0x42; 100]);
    assert!(open(original.clone()).is_ok());
    let rootblock = inode(&original, 0).inline[0].physical;
    let bytes = Arc::new(std::sync::Mutex::new(original));
    let mut fs = ReadOnlyFs::from_device(Shared(bytes.clone()), OpenOptions::default()).unwrap();
    bytes.lock().unwrap()[rootblock as usize * BLOCK + 100] ^= 1;
    assert!(fs.lookup(ROOT, b"ReadMe.txt").is_err());
    assert!(fs.read_dir(ROOT, 0, 10).is_err());
}
#[test]
fn disconnected_cycle_and_mid_validation_budget_exhaustion() {
    let original = fixture(Scenario::Clean);
    let mut b = original.clone();
    directory(&mut b, 0, |e| e.retain(|r| r.name != "nested"));
    let mut nested = inode(&b, 1);
    nested.parent = nested.id;
    set_inode(&mut b, nested.clone());
    directory(&mut b, 1, |e| {
        e.push(DirectoryRecord {
            name: "self".into(),
            child: nested.id,
        })
    });
    assert!(open(b).is_err());
    assert!(matches!(
        ReadOnlyFs::from_device(
            Spy {
                bytes: Arc::new(original),
                fail: None
            },
            OpenOptions {
                memory_limit: 12 * 1024 * 1024 + 600_000
            }
        ),
        Err(FsError::ResourceLimit)
    ));
}
