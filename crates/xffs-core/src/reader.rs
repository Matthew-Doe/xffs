//! Complete recovered-view validation behind a private read-only device facade.
use crate::{AccessMode, BlockDevice, ImageDevice, format::*, names::comparison_key};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub const DEFAULT_MEMORY_LIMIT: usize = 128 * 1024 * 1024;
pub const MAX_READ: usize = 1024 * 1024;
pub const MAX_DIR_PAGE: usize = 1024;
#[derive(Clone, Copy, Debug)]
pub struct OpenOptions {
    pub memory_limit: usize,
}
impl OpenOptions {
    pub fn with_memory_mib(mib: usize) -> Result<Self> {
        Ok(Self {
            memory_limit: mib.checked_mul(1024 * 1024).ok_or(FsError::ResourceLimit)?,
        })
    }
}
impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            memory_limit: DEFAULT_MEMORY_LIMIT,
        }
    }
}
struct Budget {
    used: usize,
    limit: usize,
}
impl Budget {
    fn charge(&mut self, n: usize) -> Result<()> {
        self.used = self.used.checked_add(n).ok_or(FsError::ResourceLimit)?;
        if self.used > self.limit {
            return Err(FsError::ResourceLimit);
        }
        Ok(())
    }
}
// Deliberately no write or flush methods. This is the only backend interface
// reachable by recovery/validation/operations, including device-spy tests.
pub(crate) struct ReadDevice<D>(pub(crate) D);
impl<D: BlockDevice> ReadDevice<D> {
    fn capacity(&self) -> u64 {
        self.0.capacity_bytes()
    }
    fn read(&mut self, offset: u64, out: &mut [u8]) -> Result<()> {
        self.0.read_at(offset, out)?;
        Ok(())
    }
    pub(crate) fn block(&mut self, n: u64) -> Result<Block> {
        let offset = n
            .checked_mul(4096)
            .ok_or(FsError::Corrupt("block offset overflow"))?;
        let mut b = [0; BLOCK];
        self.read(offset, &mut b)?;
        Ok(b)
    }
}
pub(crate) struct Metadata<D> {
    pub(crate) device: ReadDevice<D>,
    pub(crate) overlay: BTreeMap<u64, Block>,
    pub(crate) revision: FormatRevision,
}
impl<D: BlockDevice> Metadata<D> {
    pub(crate) fn block(&mut self, n: u64) -> Result<Block> {
        let b = if let Some(b) = self.overlay.get(&n) {
            *b
        } else {
            self.device.block(n)?
        };
        require(
            decode_block(&b, n)?.revision == self.revision,
            "mixed format revisions",
        )?;
        Ok(b)
    }
}
#[derive(Clone)]
pub(crate) struct Node {
    pub(crate) inode: Inode,
    pub(crate) extents: Vec<Extent>,
    pub(crate) entries: Vec<DirectoryRecord>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatFs {
    pub blocks: u64,
    pub free_blocks: u64,
    pub inodes: u64,
    pub free_inodes: u64,
    pub block_size: u32,
    pub max_name: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryPage {
    pub entries: Vec<DirectoryRecord>,
    pub next_cookie: u64,
    pub eof: bool,
}
pub struct ReadOnlyFs<D: BlockDevice = ImageDevice> {
    pub(crate) bitmap: Vec<u8>,
    pub(crate) allow_detached: bool,
    pub(crate) metadata: Metadata<D>,
    pub(crate) superblock: Superblock,
    pub(crate) nodes: BTreeMap<u64, Node>,
    pub(crate) stats: StatFs,
    diagnostics: Vec<String>,
    pub(crate) memory_used: usize,
}
impl ReadOnlyFs<ImageDevice> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, OpenOptions::default())
    }
    pub fn open_with_options(path: impl AsRef<Path>, options: OpenOptions) -> Result<Self> {
        Self::from_device(ImageDevice::open(path, AccessMode::ReadOnly)?, options)
    }
}
fn bitmap_get(bits: &[u8], n: u64) -> bool {
    bits[(n / 8) as usize] & (1 << (n % 8)) != 0
}
fn claim(bits: &mut [u8], n: u64) -> Result<()> {
    require(!bitmap_get(bits, n), "duplicate block ownership")?;
    bits[(n / 8) as usize] |= 1 << (n % 8);
    Ok(())
}
impl<D: BlockDevice> ReadOnlyFs<D> {
    /// Takes ownership of the device and never calls its write_at or flush.
    /// Device capacity/contents must remain stable while held, as for ImageDevice.
    pub fn from_device(device: D, options: OpenOptions) -> Result<Self> {
        let mut budget = Budget {
            used: 0,
            limit: options.memory_limit,
        };
        // Covers bounded transient blocks, Unicode buffers, extent resolution,
        // a read reply, directory pages and journal assembly, independently of disk.
        budget.charge(12 * 1024 * 1024)?;
        let mut device = ReadDevice(device);
        let bytes = device.capacity();
        require(bytes / 4096 >= 4096, "image too small")?;
        let mut diagnostics = Vec::new();
        let a = device
            .block(0)
            .and_then(|b| Superblock::decode(&b, 0, bytes));
        let b = device
            .block(bytes / 4096 - 1)
            .and_then(|b| Superblock::decode(&b, bytes / 4096 - 1, bytes));
        let superblock = match (a, b) {
            (Err(FsError::Unsupported), _) | (_, Err(FsError::Unsupported)) => {
                return Err(FsError::Unsupported);
            }
            (Ok(a), Ok(b)) => {
                require(a == b, "conflicting superblock copies")?;
                a
            }
            (Ok(s), Err(_)) | (Err(_), Ok(s)) => {
                diagnostics.push("Only one superblock copy is valid; read-only fallback".into());
                s
            }
            _ => return Err(FsError::Corrupt("no valid superblock")),
        };
        let l = &superblock.layout;
        for n in [1, 2] {
            if let Ok(b) = device.block(n)
                && let Ok(h) = decode_block(&b, n)
            {
                require(h.revision == superblock.revision, "mixed control revision")?;
            }
        }
        let (control, degraded) = select_control(
            device.block(1).and_then(|b| JournalControl::decode(&b, 1)),
            device.block(2).and_then(|b| JournalControl::decode(&b, 2)),
        )?;
        if degraded {
            diagnostics.push("Only one journal control is valid".into());
        }
        let mut overlay = BTreeMap::new();
        if control.committed {
            let mut checksum = 0;
            for ordinal in 0..control.count {
                let descriptor = device.block(3 + 2 * u64::from(ordinal))?;
                let desc = JournalDescriptor::decode(&descriptor, ordinal)?;
                require(
                    decode_block(&descriptor, 3 + 2 * u64::from(ordinal))?.revision
                        == superblock.revision,
                    "mixed descriptor revision",
                )?;
                let image = device.block(4 + 2 * u64::from(ordinal))?;
                require(
                    desc.sequence == control.sequence
                        && crc32c::crc32c(&image) == desc.image_checksum,
                    "journal image checksum/sequence",
                )?;
                let h = decode_block(&image, desc.target)?;
                require(
                    h.revision == superblock.revision
                        && l.metadata_target(desc.target, h.kind)
                        && !overlay.contains_key(&desc.target),
                    "duplicate or forbidden journal target",
                )?;
                checksum = crc32c::crc32c_append(checksum, &descriptor);
                checksum = crc32c::crc32c_append(checksum, &image);
                budget.charge(BLOCK + 256)?;
                overlay.insert(desc.target, image);
            }
            require(checksum == control.checksum, "transaction checksum")?;
            diagnostics.push(format!(
                "Read-only overlay: {} metadata images, sequence {}",
                control.count, control.sequence
            ));
        }
        let mut metadata = Metadata {
            device,
            overlay,
            revision: superblock.revision,
        };
        let bitbytes = usize::try_from(l.blocks.div_ceil(8)).map_err(|_| FsError::ResourceLimit)?;
        budget.charge(bitbytes.checked_mul(2).ok_or(FsError::ResourceLimit)?)?;
        let mut bitmap = Vec::new();
        bitmap
            .try_reserve_exact(bitbytes)
            .map_err(|_| FsError::ResourceLimit)?;
        bitmap.resize(bitbytes, 0);
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(bitbytes)
            .map_err(|_| FsError::ResourceLimit)?;
        owned.resize(bitbytes, 0);
        for n in 0..l.bitmap_blocks {
            let b = metadata.block(515 + n)?;
            let h = decode_block(&b, 515 + n)?;
            require(
                h.kind == Kind::Bitmap && h.used == PAYLOAD,
                "bitmap payload",
            )?;
            let start = usize::try_from(n)
                .map_err(|_| FsError::ResourceLimit)?
                .checked_mul(PAYLOAD)
                .ok_or(FsError::ResourceLimit)?;
            let count = (bitbytes - start).min(PAYLOAD);
            bitmap[start..start + count].copy_from_slice(&b[64..64 + count]);
            require(
                b[64 + count..].iter().all(|&x| x == 0),
                "bitmap trailing bits",
            )?;
        }
        if l.blocks % 8 != 0 {
            require(
                bitmap[bitbytes - 1] >> (l.blocks % 8) == 0,
                "bitmap high bits",
            )?;
        }
        for n in 0..l.data_start {
            claim(&mut owned, n)?;
        }
        claim(&mut owned, l.blocks - 1)?;
        let mut nodes = BTreeMap::new();
        let mut retired = 0;
        for n in 0..l.table_blocks {
            let physical = l.table_start + n;
            let b = metadata.block(physical)?;
            let h = decode_block(&b, physical)?;
            require(
                h.kind == Kind::Inodes && h.used == 3840,
                "inode table payload",
            )?;
            for slot in 0..15usize {
                let index = n * 15 + slot as u64;
                let raw = &b[64 + slot * 256..64 + (slot + 1) * 256];
                if index >= l.inodes {
                    require(raw.iter().all(|&x| x == 0), "inode table padding")?;
                    continue;
                }
                if raw[16] == 0 && raw[8..16] == u64::MAX.to_le_bytes() {
                    retired += 1;
                }
                if let Some(inode) = Inode::decode_revision(raw, index, superblock.revision)? {
                    budget.charge(1024 + inode.extent_count * 64 + MAX_CHAIN * 8)?;
                    let (extents, chain) = resolve_extents(&inode, l, |n| metadata.block(n))?;
                    for n in chain {
                        claim(&mut owned, n)?;
                    }
                    for e in &extents {
                        for n in e.physical..e.physical + e.length {
                            claim(&mut owned, n)?;
                        }
                    }
                    nodes.insert(
                        index,
                        Node {
                            inode,
                            extents,
                            entries: Vec::new(),
                        },
                    );
                }
            }
        }
        require(
            owned == bitmap,
            "unallocated reference or unexplained allocation",
        )?;
        let root = nodes.get(&0).ok_or(FsError::Corrupt("missing root"))?;
        require(
            root.inode.id == ROOT
                && root.inode.parent == ROOT
                && root.inode.kind == FileKind::Directory
                && root.inode.state == InodeState::Linked,
            "root identity",
        )?;
        // Every overlay allocatable target must actually be metadata of its
        // declared owner, not a free block or a file's raw data masquerading as metadata.
        for (&n, b) in &metadata.overlay {
            let h = decode_block(b, n)?;
            if l.allocatable(n) {
                let owner = nodes
                    .get(&h.owner.index)
                    .ok_or(FsError::Corrupt("journal owner missing"))?;
                require(owner.inode.id == h.owner, "journal owner generation")?;
                let valid = match h.kind {
                    Kind::Directory => {
                        owner.inode.kind == FileKind::Directory
                            && owner
                                .extents
                                .iter()
                                .any(|e| n >= e.physical && n < e.physical + e.length)
                    }
                    Kind::Extents => true,
                    _ => false,
                };
                // Extent-chain membership is checked below through overlay (not home).
                if h.kind != Kind::Extents {
                    require(valid, "journal target is not owned metadata")?;
                }
            }
        }
        // Directory keys and references are validated before exposing any node.
        let indices: Vec<u64> = nodes.keys().copied().collect(); // budgeted per node above
        let mut referenced = BTreeSet::new();
        let mut metadata_extents = BTreeSet::new();
        for &index in &indices {
            let node = &nodes[&index];
            let (_, chain) = resolve_extents(&node.inode, l, |n| metadata.block(n))?;
            metadata_extents.extend(chain);
            if node.inode.kind != FileKind::Directory {
                continue;
            }
            let id = node.inode.id;
            let mut keys = BTreeSet::new();
            let mut entries = Vec::new();
            for e in &node.extents {
                for n in e.physical..e.physical + e.length {
                    for r in decode_directory(&metadata.block(n)?, n, id)? {
                        let key = comparison_key(r.name.as_bytes())?;
                        budget.charge(512 + r.name.len() * 2 + key.len() * 2)?;
                        require(keys.insert(key), "equivalent duplicate names")?;
                        let child = nodes
                            .get(&r.child.index)
                            .ok_or(FsError::Corrupt("directory references unused inode"))?;
                        require(
                            child.inode.id == r.child
                                && child.inode.state == InodeState::Linked
                                && child.inode.parent == id
                                && r.child != ROOT,
                            "stale or invalid directory reference",
                        )?;
                        require(
                            referenced.insert(r.child.index),
                            "multiple namespace references",
                        )?;
                        entries.push(r);
                    }
                }
            }
            require(
                node.inode.state == InodeState::Linked || entries.is_empty(),
                "nonempty detached directory",
            )?;
            nodes
                .get_mut(&index)
                .ok_or(FsError::Corrupt("missing directory"))?
                .entries = entries;
        }
        for (&n, b) in &metadata.overlay {
            if decode_block(b, n)?.kind == Kind::Extents {
                require(
                    metadata_extents.contains(&n),
                    "journal target not an extent block",
                )?;
            }
        }
        for &index in &indices {
            let i = &nodes[&index].inode;
            if i.state == InodeState::Linked && index != 0 {
                require(referenced.contains(&index), "unreachable linked inode")?;
            }
        }
        // Iterative tree walk rejects detached cycles without recursion or O(n²) walks.
        let mut reached = BTreeSet::new();
        let mut pending = vec![ROOT];
        while let Some(id) = pending.pop() {
            require(reached.insert(id.index), "directory cycle")?;
            let node = &nodes[&id.index];
            for r in &node.entries {
                pending.push(r.child);
            }
        }
        require(
            nodes
                .values()
                .filter(|n| n.inode.state == InodeState::Linked)
                .count()
                == reached.len(),
            "disconnected directory cycle",
        )?;
        let used = bitmap
            .iter()
            .map(|b| u64::from(b.count_ones()))
            .sum::<u64>();
        let stats = StatFs {
            blocks: l.blocks,
            free_blocks: l.blocks - used,
            inodes: l.inodes,
            free_inodes: l.inodes - nodes.len() as u64 - retired,
            block_size: 4096,
            max_name: 255,
        };
        Ok(Self {
            bitmap,
            allow_detached: false,
            metadata,
            superblock,
            nodes,
            stats,
            diagnostics,
            memory_used: budget.used,
        })
    }
    pub fn superblock(&self) -> &Superblock {
        &self.superblock
    }
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
    pub fn memory_used(&self) -> usize {
        self.memory_used
    }
    pub fn statfs(&self) -> StatFs {
        self.stats
    }
    pub fn recovered_blocks(&self) -> usize {
        self.metadata.overlay.len()
    }
    /// Converts a FUSE inode index to a currently linked generation-bearing ID.
    pub fn inode_id(&self, index: u64) -> Result<InodeId> {
        let n = self.nodes.get(&index).ok_or(FsError::NotFound)?;
        if n.inode.state != InodeState::Linked {
            return Err(FsError::NotFound);
        }
        Ok(n.inode.id)
    }
    pub(crate) fn node(&self, id: InodeId) -> Result<&Node> {
        let n = self.nodes.get(&id.index).ok_or(FsError::NotFound)?;
        if n.inode.id != id {
            return Err(FsError::Stale);
        }
        if n.inode.state != InodeState::Linked && !self.allow_detached {
            return Err(FsError::NotFound);
        }
        Ok(n)
    }
    fn validate_node(&mut self, id: InodeId) -> Result<()> {
        self.node(id)?;
        let block = self.superblock.layout.table_start + id.index / 15;
        let b = self.metadata.block(block)?;
        let h = decode_block(&b, block)?;
        require(
            h.kind == Kind::Inodes && h.used == 3840,
            "inode table payload",
        )?;
        let slot = (id.index % 15) as usize;
        let current = Inode::decode_revision(
            &b[64 + slot * 256..64 + (slot + 1) * 256],
            id.index,
            self.superblock.revision,
        )?
        .ok_or(FsError::Stale)?;
        require(
            current == self.nodes[&id.index].inode,
            "inode changed while mounted",
        )?;
        let (extents, _) = resolve_extents(&current, &self.superblock.layout, |n| {
            self.metadata.block(n)
        })?;
        require(
            extents == self.nodes[&id.index].extents,
            "extents changed while mounted",
        )?;
        Ok(())
    }
    fn validate_directory(&mut self, id: InodeId) -> Result<()> {
        self.validate_node(id)?;
        let node = &self.nodes[&id.index];
        if node.inode.kind != FileKind::Directory {
            return Err(FsError::NotDirectory);
        }
        let mut pos = 0;
        for e in &node.extents {
            for n in e.physical..e.physical + e.length {
                let entries = decode_directory(&self.metadata.block(n)?, n, id)?;
                let end = pos + entries.len();
                require(
                    node.entries.get(pos..end) == Some(entries.as_slice()),
                    "directory changed while mounted",
                )?;
                pos = end;
            }
        }
        require(pos == node.entries.len(), "directory changed length")
    }
    pub fn getattr(&mut self, id: InodeId) -> Result<Inode> {
        self.validate_node(id)?;
        Ok(self.nodes[&id.index].inode.clone())
    }
    pub fn lookup(&mut self, parent: InodeId, name: &[u8]) -> Result<InodeId> {
        let key = comparison_key(name)?;
        self.validate_directory(parent)?;
        for r in &self.nodes[&parent.index].entries {
            if comparison_key(r.name.as_bytes())? == key {
                return Ok(r.child);
            }
        }
        Err(FsError::NotFound)
    }
    /// Cookies are zero-based entry positions, excluding synthetic dot entries.
    pub fn read_dir(&mut self, id: InodeId, cookie: u64, limit: usize) -> Result<DirectoryPage> {
        if limit == 0 || limit > MAX_DIR_PAGE {
            return Err(FsError::ResourceLimit);
        }
        self.validate_directory(id)?;
        let entries = &self.nodes[&id.index].entries;
        let start = usize::try_from(cookie).map_err(|_| FsError::Corrupt("directory cookie"))?;
        require(start <= entries.len(), "directory cookie")?;
        let end = start.saturating_add(limit).min(entries.len());
        Ok(DirectoryPage {
            entries: entries[start..end].to_vec(),
            next_cookie: end as u64,
            eof: end == entries.len(),
        })
    }
    pub fn read_file(&mut self, id: InodeId, offset: u64, length: usize) -> Result<Vec<u8>> {
        if length > MAX_READ {
            return Err(FsError::ResourceLimit);
        }
        self.validate_node(id)?;
        let node = &self.nodes[&id.index];
        if node.inode.kind != FileKind::File {
            return Err(FsError::IsDirectory);
        }
        let length = (node.inode.size.saturating_sub(offset)).min(length as u64) as usize;
        let mut out = vec![0; length];
        if length == 0 {
            return Ok(out);
        }
        let end = offset + length as u64;
        for e in &node.extents {
            let start = e.logical * 4096;
            let extent_end = e
                .logical
                .checked_add(e.length)
                .and_then(|n| n.checked_mul(4096))
                .unwrap_or(u64::MAX);
            let lo = offset.max(start);
            let hi = end.min(extent_end);
            if lo < hi {
                let physical = e.physical * 4096 + (lo - start);
                self.metadata.device.read(
                    physical,
                    &mut out[(lo - offset) as usize..(hi - offset) as usize],
                )?;
            }
        }
        Ok(out)
    }
}
