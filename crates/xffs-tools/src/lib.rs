//! Explicit image creation and deterministic fixtures, not a transaction engine.
#![forbid(unsafe_code)]
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
    path::Path,
};
use xffs_core::{AccessMode, BlockDevice, ImageDevice, format::*};
mod formatter;
pub use formatter::format_empty;

pub type ToolResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Scenario {
    Clean,
    Committed,
    PartialCheckpoint,
}
pub const DEMO_UUID: [u8; 16] = [
    0x58, 0x46, 0x46, 0x53, 0, 0, 0, 1, 0x80, 0, 0, 0, 0, 0, 0, 1,
];
pub const TIME: u64 = 1_700_000_000;
struct Builder {
    layout: VolumeLayout,
    next: u64,
    blocks: BTreeMap<u64, Block>,
    inodes: Vec<Inode>,
}
impl Builder {
    fn new(layout: VolumeLayout) -> Self {
        Self {
            next: layout.data_start,
            layout,
            blocks: BTreeMap::new(),
            inodes: Vec::new(),
        }
    }
    fn alloc(&mut self, b: Block) -> Result<u64> {
        let n = self.next;
        if !self.layout.allocatable(n) {
            return Err(FsError::ResourceLimit);
        }
        self.blocks.insert(n, b);
        self.next += 1;
        Ok(n)
    }
    fn inode(&mut self, kind: FileKind, parent: InodeId) -> Result<InodeId> {
        let id = InodeId {
            index: self.inodes.len() as u64,
            generation: 1,
        };
        if id.index >= self.layout.inodes {
            return Err(FsError::ResourceLimit);
        }
        self.inodes.push(Inode {
            cleanup_bound: 0,
            atime: 0,
            id,
            kind,
            state: InodeState::Linked,
            executable: false,
            size: 0,
            allocated: 0,
            parent,
            times: [TIME; 3],
            overflow: 0,
            extent_count: 0,
            inline: vec![],
        });
        Ok(id)
    }
    fn mapping(&mut self, id: InodeId, size: u64, extents: Vec<Extent>) -> Result<()> {
        let mut next = 0;
        for chunk in extents
            .get(4..)
            .unwrap_or(&[])
            .chunks(EXTENTS_PER_BLOCK)
            .rev()
        {
            let block = self.next;
            next = self.alloc(encode_extents(block, id, next, chunk)?)?;
        }
        let i = &mut self.inodes[id.index as usize];
        i.size = size;
        i.extent_count = extents.len();
        i.allocated = extents.iter().map(|e| e.length).sum::<u64>()
            + extents.len().saturating_sub(4).div_ceil(EXTENTS_PER_BLOCK) as u64;
        i.overflow = next;
        i.inline = extents.into_iter().take(4).collect();
        Ok(())
    }
    fn file(&mut self, parent: InodeId, data: &[u8], executable: bool) -> Result<InodeId> {
        let id = self.inode(FileKind::File, parent)?;
        let mut extents = vec![];
        for (n, chunk) in data.chunks(BLOCK).enumerate() {
            let mut b = [0; BLOCK];
            b[..chunk.len()].copy_from_slice(chunk);
            let physical = self.alloc(b)?;
            extents.push(Extent {
                logical: n as u64,
                physical,
                length: 1,
            });
        }
        self.mapping(id, data.len() as u64, extents)?;
        self.inodes[id.index as usize].executable = executable;
        Ok(id)
    }
    fn directory(&mut self, id: InodeId, entries: Vec<DirectoryRecord>) -> Result<()> {
        let mut payload = Vec::new();
        let mut extents = vec![];
        for entry in entries {
            let b = entry.encode()?;
            if payload.len() + b.len() > PAYLOAD {
                self.dir_block(id, &mut payload, &mut extents)?;
            }
            payload.extend(b);
        }
        if !payload.is_empty() {
            self.dir_block(id, &mut payload, &mut extents)?;
        }
        self.mapping(id, extents.len() as u64 * 4096, extents)
    }
    fn dir_block(
        &mut self,
        id: InodeId,
        payload: &mut Vec<u8>,
        extents: &mut Vec<Extent>,
    ) -> Result<()> {
        let n = self.next;
        self.alloc(encode_block(Kind::Directory, n, id, payload)?)?;
        extents.push(Extent {
            logical: extents.len() as u64,
            physical: n,
            length: 1,
        });
        payload.clear();
        Ok(())
    }
    fn table(&self, n: u64) -> Result<Block> {
        let mut p = [0; 3840];
        for slot in 0..15 {
            let index = n * 15 + slot;
            if let Some(i) = self.inodes.get(index as usize) {
                p[slot as usize * 256..(slot as usize + 1) * 256].copy_from_slice(&i.encode()?);
            }
        }
        encode_block(
            Kind::Inodes,
            self.layout.table_start + n,
            InodeId::default(),
            &p,
        )
    }
}
fn entry(name: &str, child: InodeId) -> DirectoryRecord {
    DirectoryRecord {
        name: name.into(),
        child,
    }
}
/// Preflights all geometry/fixture content before create_new. Removes only the
/// newly created path on failure; existing paths are never truncated or removed.
pub fn create_image(
    path: &Path,
    bytes: u64,
    uuid: [u8; 16],
    inodes: Option<u64>,
    scenario: Option<Scenario>,
) -> ToolResult<()> {
    create_image_revision(path, bytes, uuid, inodes, scenario, FormatRevision::Two)
}
pub fn create_image_revision(
    path: &Path,
    bytes: u64,
    uuid: [u8; 16],
    inodes: Option<u64>,
    scenario: Option<Scenario>,
    revision: FormatRevision,
) -> ToolResult<()> {
    let layout = VolumeLayout::new(bytes, inodes)?;
    if scenario.is_none() {
        return create_new_image(path, bytes, |file| {
            format_empty(
                &mut formatter::NewImage {
                    file,
                    capacity: bytes,
                },
                uuid,
                inodes,
                revision,
            )?;
            Ok(())
        });
    }
    let mut b = Builder::new(layout.clone());
    b.inode(FileKind::Directory, ROOT)?;
    let mut journal = Vec::new();
    if let Some(scenario) = scenario {
        let nested = b.inode(FileKind::Directory, ROOT)?;
        let empty = b.inode(FileKind::Directory, nested)?;
        let text = b.file(ROOT, b"Hello from XFFS!\n", false)?;
        let unicode = b.file(ROOT, b"Canonical caseless lookup.\n", false)?;
        let exec = b.file(ROOT, b"#!/bin/sh\nprintf 'XFFS demo\\n'\n", true)?;
        let binary: Vec<u8> = (0..8192).map(|n| (n % 256) as u8).collect();
        let bin = b.file(nested, &binary, false)?;
        let overflow = b.file(ROOT, &vec![0x5a; 5 * BLOCK], false)?;
        let sparse = b.inode(FileKind::File, ROOT)?;
        let first = b.alloc([0x11; BLOCK])?;
        let last = b.alloc([0x77; BLOCK])?;
        b.mapping(
            sparse,
            (1u64 << 32) + 4096,
            vec![
                Extent {
                    logical: 0,
                    physical: first,
                    length: 1,
                },
                Extent {
                    logical: 1 << 20,
                    physical: last,
                    length: 1,
                },
            ],
        )?;
        let empty_file = b.file(ROOT, b"", false)?;
        let many = b.inode(FileKind::Directory, ROOT)?;
        let mut entries = vec![];
        for n in 0..160 {
            entries.push(entry(
                &format!("entry-{n:03}.txt"),
                b.file(many, b"", false)?,
            ));
        }
        b.directory(many, entries)?;
        b.directory(
            nested,
            vec![entry("empty-dir", empty), entry("binary.bin", bin)],
        )?;
        let marker = b.file(ROOT, b"Journal recovery is visible.\n", false)?;
        b.directory(
            ROOT,
            vec![
                entry("nested", nested),
                entry("ReadMe.txt", text),
                entry("Café.txt", unicode),
                entry("run.sh", exec),
                entry("overflow.bin", overflow),
                entry("sparse.bin", sparse),
                entry("empty.txt", empty_file),
                entry("many", many),
                entry("Recovered.txt", marker),
            ],
        )?;
        let rootblock = b.inodes[0].inline[0].physical;
        let final_dir = b.blocks[&rootblock];
        if scenario != Scenario::Clean {
            let final_table = b.table(0)?;
            let records = decode_directory(&final_dir, rootblock, ROOT)?;
            let mut p = Vec::new();
            for mut r in records {
                if r.name == "Recovered.txt" {
                    r.name = "Before.txt".into();
                }
                p.extend(r.encode()?);
            }
            b.blocks.insert(
                rootblock,
                encode_block(Kind::Directory, rootblock, ROOT, &p)?,
            );
            b.inodes[0].times = [TIME - 1; 3];
            journal.push((rootblock, final_dir));
            journal.push((layout.table_start, final_table));
            if scenario == Scenario::PartialCheckpoint {
                b.blocks.insert(rootblock, final_dir);
            }
        }
    }
    let sb = Superblock {
        revision,
        uuid,
        layout: layout.clone(),
    };
    let primary = sb.encode(0)?;
    let backup = sb.encode(layout.blocks - 1)?;
    create_new_image(path, bytes, |file| {
        let mut write = |block: u64, bytes: &Block| -> std::io::Result<()> {
            file.seek(SeekFrom::Start(block * 4096))?;
            file.write_all(bytes)
        };
        write(0, &primary)?;
        write(layout.blocks - 1, &backup)?;
        let mut checksum = 0;
        for (n, (target, image)) in journal.iter().enumerate() {
            let image = &with_revision(*image, revision);
            let descriptor = with_revision(
                JournalDescriptor {
                    target: *target,
                    ordinal: n as u32,
                    image_checksum: crc32c::crc32c(image),
                    sequence: 2,
                }
                .encode()?,
                revision,
            );
            checksum = crc32c::crc32c_append(checksum, &descriptor);
            checksum = crc32c::crc32c_append(checksum, image);
            write(3 + 2 * n as u64, &descriptor)?;
            write(4 + 2 * n as u64, image)?;
        }
        let control = JournalControl {
            sequence: if journal.is_empty() { 1 } else { 2 },
            committed: !journal.is_empty(),
            count: journal.len() as u32,
            checksum,
        };
        for n in [1, 2] {
            write(n, &with_revision(control.encode(n)?, revision))?;
        }
        for n in 0..layout.bitmap_blocks {
            let mut p = [0; PAYLOAD];
            for bit in 0..32256 {
                let absolute = n * 32256 + bit;
                if absolute < layout.blocks && (absolute < b.next || absolute == layout.blocks - 1)
                {
                    p[bit as usize / 8] |= 1 << (bit % 8);
                }
            }
            write(
                515 + n,
                &with_revision(
                    encode_block(Kind::Bitmap, 515 + n, InodeId::default(), &p)?,
                    revision,
                ),
            )?;
        }
        for n in 0..layout.table_blocks {
            write(
                layout.table_start + n,
                &with_revision(b.table(n)?, revision),
            )?;
        }
        for (n, block) in &b.blocks {
            let metadata = b.inodes.iter().any(|i| {
                i.overflow == *n
                    || (i.kind == FileKind::Directory
                        && i.inline
                            .iter()
                            .any(|e| *n >= e.physical && *n < e.physical + e.length))
            });
            write(
                *n,
                &if metadata {
                    with_revision(*block, revision)
                } else {
                    *block
                },
            )?;
        }
        Ok(())
    })
}

fn create_new_image(
    path: &Path,
    bytes: u64,
    write: impl FnOnce(&mut std::fs::File) -> ToolResult<()>,
) -> ToolResult<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let result = (|| {
        file.try_lock()?;
        file.set_len(bytes)?;
        write(&mut file)?;
        file.sync_all()?;
        Ok(())
    })();
    drop(file);
    if result.is_err() {
        std::fs::remove_file(path)?;
    }
    result
}

fn inspection_budget(out: &str) -> ToolResult<()> {
    if out.len() > 8 * 1024 * 1024 {
        return Err(FsError::ResourceLimit.into());
    }
    Ok(())
}

/// Forensic HOME view, explicitly not recovery or mount validation.
pub fn inspect(path: &Path) -> ToolResult<String> {
    inspect_device(ImageDevice::open(path, AccessMode::ReadOnly)?)
}

pub fn inspect_device(mut d: impl BlockDevice) -> ToolResult<String> {
    let bytes = d.capacity_bytes();
    if bytes / 4096 < 4096 {
        return Err("image too small".into());
    }
    let mut out = String::from("RAW HOME metadata (not the recovered mounted namespace)\n");
    let mut sb = None;
    for n in [0, bytes / 4096 - 1] {
        let mut b = [0; BLOCK];
        d.read_at(n * 4096, &mut b)?;
        let s = Superblock::decode(&b, n, bytes);
        out.push_str(&format!("superblock {n}: {s:?}\n"));
        if let Ok(s) = s {
            sb = Some(s);
        }
    }
    for n in [1, 2] {
        let mut b = [0; BLOCK];
        d.read_at(n * 4096, &mut b)?;
        out.push_str(&format!(
            "HOME control {n}: {:?}\n",
            JournalControl::decode(&b, n)
        ));
    }
    let s = sb.ok_or("no valid superblock")?;
    for n in 515..s.layout.table_start {
        inspection_budget(&out)?;
        let mut b = [0; BLOCK];
        d.read_at(n * 4096, &mut b)?;
        let h = decode_block(&b, n)?;
        if h.kind != Kind::Bitmap || h.used != PAYLOAD {
            return Err("bad bitmap".into());
        }
        let allocated: u32 = b[64..].iter().map(|b| b.count_ones()).sum();
        out.push_str(&format!("HOME bitmap {n}: {allocated} set bits\n"));
    }
    for n in 0..s.layout.table_blocks {
        let physical = s.layout.table_start + n;
        let mut b = [0; BLOCK];
        d.read_at(physical * 4096, &mut b)?;
        let h = decode_block(&b, physical)?;
        if h.kind != Kind::Inodes || h.used != 3840 {
            return Err("bad inode table".into());
        }
        for slot in 0..15 {
            inspection_budget(&out)?;
            let index = n * 15 + slot;
            if index >= s.layout.inodes {
                break;
            }
            if let Some(i) = Inode::decode_revision(
                &b[64 + slot as usize * 256..64 + (slot as usize + 1) * 256],
                index,
                s.revision,
            )? {
                out.push_str(&format!("HOME inode: {i:?}\n"));
                let (extents, chain) = resolve_extents(&i, &s.layout, |block| {
                    let mut b = [0; BLOCK];
                    d.read_at(block * 4096, &mut b)?;
                    Ok(b)
                })?;
                out.push_str(&format!("HOME overflow blocks: {chain:?}\n"));
                if i.kind == FileKind::Directory {
                    for e in extents {
                        for block in e.physical..e.physical + e.length {
                            inspection_budget(&out)?;
                            let mut b = [0; BLOCK];
                            d.read_at(block * 4096, &mut b)?;
                            out.push_str(&format!(
                                "HOME directory block {block}: {:?}\n",
                                decode_directory(&b, block, i.id)?
                            ));
                        }
                    }
                }
            }
        }
    }
    for n in 0..256 {
        inspection_budget(&out)?;
        let mut b = [0; BLOCK];
        d.read_at((3 + 2 * n) * 4096, &mut b)?;
        if b.iter().all(|&x| x == 0) {
            continue;
        }
        out.push_str(&format!(
            "JOURNAL PAYLOAD descriptor {n}: {:?}\n",
            JournalDescriptor::decode(&b, n as u32)
        ));
        if let Ok(desc) = JournalDescriptor::decode(&b, n as u32) {
            d.read_at((4 + 2 * n) * 4096, &mut b)?;
            let h = decode_block(&b, desc.target);
            out.push_str(&format!("JOURNAL PAYLOAD image {n}: {h:?}\n"));
            if let Ok(h) = h
                && h.kind == Kind::Directory
            {
                out.push_str(&format!(
                    "JOURNAL PAYLOAD directory: {:?}\n",
                    decode_directory(&b, desc.target, h.owner)?
                ));
            }
        }
    }
    Ok(out)
}

/// A serial must be present and nonempty; loop devices use the formatter API.
pub fn validate_serial(info: &xffs_core::DeviceInfo, expected: &str) -> ToolResult<()> {
    if expected.is_empty() || info.serial.as_deref() != Some(expected) {
        return Err("expected serial does not match the claimed device; nothing written".into());
    }
    Ok(())
}

/// The formatter claim must have been released. Never restore partition tables.
pub fn refresh_partition_view(path: &Path, expected: &xffs_core::DeviceInfo) -> ToolResult<()> {
    refresh_partition_view_inner(path, expected).map_err(|error| {
        format!("filesystem writing completed, but post-format verification failed: {error}; do not repeat formatting automatically").into()
    })
}

fn refresh_partition_view_inner(path: &Path, expected: &xffs_core::DeviceInfo) -> ToolResult<()> {
    use xffs_core::LinuxBlockDevice;
    let start = std::time::Instant::now();
    let mut claim = || {
        let device = LinuxBlockDevice::open(path, AccessMode::ReadOnly)?;
        let info = device.info().clone();
        Ok((device, info))
    };
    let mut now = || start.elapsed();
    let mut sleep = std::thread::sleep;
    drop(claim_matching(expected, &mut claim, &mut now, &mut sleep)?);
    let status = std::process::Command::new("blockdev")
        .arg("--rereadpt")
        .arg(path)
        .status()?;
    if !status.success() {
        return Err("kernel partition refresh failed; do not mount stale partitions".into());
    }
    let _check = claim_matching(expected, &mut claim, &mut now, &mut sleep)?;
    let sysfs = std::path::PathBuf::from(format!(
        "/sys/dev/block/{}:{}",
        expected.major, expected.minor
    ));
    for entry in std::fs::read_dir(sysfs)? {
        if entry?.path().join("partition").exists() {
            return Err("format finished, but kernel still exposes partitions".into());
        }
    }
    Ok(())
}

/// Each claim gets its own deadline. Only typed contention is retryable.
fn claim_matching<T>(
    expected: &xffs_core::DeviceInfo,
    claim: &mut impl FnMut() -> std::result::Result<(T, xffs_core::DeviceInfo), xffs_core::DeviceError>,
    now: &mut impl FnMut() -> std::time::Duration,
    sleep: &mut impl FnMut(std::time::Duration),
) -> std::result::Result<T, xffs_core::DeviceError> {
    use std::time::Duration;
    use xffs_core::DeviceError;
    let deadline = now() + Duration::from_secs(15);
    loop {
        match claim() {
            Ok((device, info)) => {
                return if &info == expected {
                    Ok(device)
                } else {
                    Err(DeviceError::IdentityChanged)
                };
            }
            Err(DeviceError::LockContention) => {
                let current = now();
                if current >= deadline {
                    return Err(DeviceError::LockContention);
                }
                sleep(Duration::from_millis(100).min(deadline - current));
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn open_target(
    path: &Path,
    device: bool,
    access: AccessMode,
) -> ToolResult<Box<dyn BlockDevice + Send>> {
    open_target_identified(path, device, access, None, None)
}

pub fn open_target_identified(
    path: &Path,
    device: bool,
    access: AccessMode,
    serial: Option<&str>,
    disk_sequence: Option<u64>,
) -> ToolResult<Box<dyn BlockDevice + Send>> {
    if device {
        let device = xffs_core::LinuxBlockDevice::open(path, access)?;
        device.require_identity(serial, disk_sequence)?;
        Ok(Box::new(device))
    } else {
        if serial.is_some() || disk_sequence.is_some() {
            return Err("identity expectations require --device".into());
        }
        Ok(Box::new(ImageDevice::open(path, access)?))
    }
}

/// Validate both copies of an existing format without writing or repairing it.
pub fn verify_existing_format(
    device: &mut impl BlockDevice,
    uuid: [u8; 16],
    inodes: u64,
) -> ToolResult<()> {
    let capacity = device.capacity_bytes();
    if capacity < 4096 {
        return Err("device too small".into());
    }
    let mut copies = Vec::new();
    for block in [0, capacity / 4096 - 1] {
        let mut bytes = [0; BLOCK];
        device.read_at(block * 4096, &mut bytes)?;
        copies.push(Superblock::decode(&bytes, block, capacity)?);
    }
    if copies[0] != copies[1]
        || copies[0].uuid != uuid
        || copies[0].revision != FormatRevision::Two
        || copies[0].layout.inodes != inodes
    {
        return Err(
            "existing format does not match expected UUID, revision, inode count, or redundancy"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, time::Duration};
    use xffs_core::{DeviceError, DeviceInfo};

    fn identity() -> DeviceInfo {
        DeviceInfo {
            capacity: 32 << 20,
            logical_sector: 512,
            physical_sector: 512,
            major: 7,
            minor: 0,
            serial: None,
            usb: false,
            removable: false,
            disk_sequence: 42,
        }
    }

    #[test]
    fn claim_retries_transient_contention_with_independent_deadlines() {
        let clock = Cell::new(Duration::ZERO);
        for _ in 0..2 {
            let mut attempts = 0;
            let start = clock.get();
            super::claim_matching(
                &identity(),
                &mut || {
                    attempts += 1;
                    if attempts < 4 {
                        Err(DeviceError::LockContention)
                    } else {
                        Ok(((), identity()))
                    }
                },
                &mut || clock.get(),
                &mut |delay| {
                    assert_eq!(delay, Duration::from_millis(100));
                    clock.set(clock.get() + delay);
                },
            )
            .unwrap();
            assert_eq!(attempts, 4);
            assert_eq!(clock.get() - start, Duration::from_millis(300));
        }
    }

    #[test]
    fn claim_contention_times_out() {
        let clock = Cell::new(Duration::ZERO);
        let mut attempts = 0;
        let result = super::claim_matching::<()>(
            &identity(),
            &mut || {
                attempts += 1;
                Err(DeviceError::LockContention)
            },
            &mut || clock.get(),
            &mut |delay| clock.set(clock.get() + delay),
        );
        assert!(matches!(result, Err(DeviceError::LockContention)));
        assert_eq!(clock.get(), Duration::from_secs(15));
        assert_eq!(attempts, 151);
    }

    #[test]
    fn claim_rejects_replacement_and_permanent_errors_immediately() {
        let mut changed = identity();
        changed.disk_sequence += 1;
        let result = super::claim_matching(
            &identity(),
            &mut || Ok(((), changed.clone())),
            &mut || Duration::ZERO,
            &mut |_| panic!("must not retry replacement"),
        );
        assert!(matches!(result, Err(DeviceError::IdentityChanged)));
        for error in [
            DeviceError::IdentityChanged,
            DeviceError::UnsupportedGeometry,
            DeviceError::UnsafeTopology("mounted".into()),
        ] {
            let mut error = Some(error);
            assert!(
                super::claim_matching::<()>(
                    &identity(),
                    &mut || Err(error.take().unwrap()),
                    &mut || Duration::ZERO,
                    &mut |_| panic!("must not retry permanent error")
                )
                .is_err()
            );
        }
    }

    #[test]
    fn incomplete_creation_is_removed_and_existing_path_is_preserved() {
        let p = std::env::temp_dir().join(format!("xffs-failure-{}.img", std::process::id()));
        let result = super::create_new_image(&p, 4096, |file| {
            use std::io::Write;
            file.write_all(b"partial")?;
            Err("injected write failure".into())
        });
        assert!(result.is_err());
        assert!(!p.exists());
        std::fs::write(&p, b"existing").unwrap();
        assert!(
            super::create_new_image(&p, 4096, |_| panic!("must refuse before writing")).is_err()
        );
        assert_eq!(std::fs::read(&p).unwrap(), b"existing");
        std::fs::remove_file(p).unwrap();
    }
}
