//! Serialized durable image writer. No device I/O is exposed to callers.
use crate::{
    AccessMode, BlockDevice, ImageDevice, ReadOnlyFs,
    format::*,
    reader::{DirectoryPage, MAX_READ, Node, OpenOptions, StatFs},
};
use std::{collections::BTreeMap, path::Path};

pub struct ReadWriteFs<D: BlockDevice = ImageDevice> {
    pub(crate) view: ReadOnlyFs<D>,
    sequence: u64,
    faulted: bool,
    memory_limit: usize,
    next_block: u64,
    opens: BTreeMap<InodeId, u64>,
}
impl ReadWriteFs<ImageDevice> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, OpenOptions::default())
    }
    pub fn open_with_options(path: impl AsRef<Path>, options: OpenOptions) -> Result<Self> {
        Self::from_device(ImageDevice::open(path, AccessMode::ReadWrite)?, options)
    }
}
impl<D: BlockDevice> ReadWriteFs<D> {
    pub fn from_device(device: D, options: OpenOptions) -> Result<Self> {
        // Complete recovered-view validation happens before the first write/flush.
        let mut view = ReadOnlyFs::from_device(device, options)?;
        if view.superblock.revision != FormatRevision::Two {
            return Err(FsError::Unsupported);
        }
        let bytes = view.metadata.device.0.capacity_bytes();
        for n in [0, view.superblock.layout.blocks - 1] {
            require(
                Superblock::decode(&view.metadata.device.block(n)?, n, bytes)? == view.superblock,
                "writable superblock redundancy",
            )?;
        }
        let (control, _) = select_control(
            view.metadata
                .device
                .block(1)
                .and_then(|b| JournalControl::decode(&b, 1)),
            view.metadata
                .device
                .block(2)
                .and_then(|b| JournalControl::decode(&b, 2)),
        )?;
        if control.committed && control.sequence == u64::MAX {
            return Err(FsError::CounterExhausted);
        }
        view.memory_used = 12 * 1024 * 1024
            + view.bitmap.len() * 2
            + view.nodes.values().map(node_memory).sum::<usize>();
        if view
            .memory_used
            .checked_add(STAGING_RESERVE)
            .is_none_or(|n| n > options.memory_limit)
        {
            return Err(FsError::ResourceLimit);
        }
        view.allow_detached = true;
        let mut fs = Self {
            next_block: view.superblock.layout.data_start,
            opens: BTreeMap::new(),
            view,
            sequence: control.sequence,
            faulted: false,
            memory_limit: options.memory_limit,
        };
        // Repair both controls to the selected state before any home writes or reuse.
        fs.controls(control)?;
        if control.committed {
            let overlay = std::mem::take(&mut fs.view.metadata.overlay);
            for (n, b) in overlay {
                fs.device().write_at(n * 4096, &b)?;
            }
            fs.device().flush()?;
            fs.sequence += 1;
            fs.controls(JournalControl {
                sequence: fs.sequence,
                committed: false,
                count: 0,
                checksum: 0,
            })?;
        }
        fs.resume_cleanup()?;
        Ok(fs)
    }
    fn device(&mut self) -> &mut D {
        &mut self.view.metadata.device.0
    }
    pub(crate) fn healthy(&self) -> Result<()> {
        if self.faulted {
            Err(FsError::Faulted)
        } else {
            Ok(())
        }
    }
    fn controls(&mut self, c: JournalControl) -> Result<()> {
        for n in [1, 2] {
            self.device()
                .write_at(n * 4096, &with_revision(c.encode(n)?, FormatRevision::Two))?;
            self.device().flush()?;
        }
        Ok(())
    }
    /// Callers validate affected ownership and reserve all resources before entry.
    pub(crate) fn commit(
        &mut self,
        images: &BTreeMap<u64, Block>,
        data: &[(u64, Block)],
    ) -> Result<()> {
        self.healthy()?;
        if images.is_empty() || images.len() > 256 {
            return Err(FsError::TooBig);
        }
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(FsError::CounterExhausted)?;
        let clean_sequence = sequence.checked_add(1).ok_or(FsError::CounterExhausted)?;
        let transient = (images.len() * 3 + data.len()) * (BLOCK + 256);
        if self
            .view
            .memory_used
            .checked_add(transient)
            .is_none_or(|n| n > self.memory_limit)
        {
            return Err(FsError::ResourceLimit);
        }
        let mut payload = Vec::with_capacity(images.len());
        let mut checksum = 0;
        for (ordinal, (&target, image)) in images.iter().enumerate() {
            let h = decode_block(image, target)?;
            require(
                h.revision == FormatRevision::Two
                    && self.view.superblock.layout.metadata_target(target, h.kind),
                "transaction metadata target",
            )?;
            let descriptor = with_revision(
                JournalDescriptor {
                    target,
                    ordinal: ordinal as u32,
                    image_checksum: crc32c::crc32c(image),
                    sequence,
                }
                .encode()?,
                FormatRevision::Two,
            );
            checksum = crc32c::crc32c_append(checksum, &descriptor);
            checksum = crc32c::crc32c_append(checksum, image);
            payload.push(descriptor);
        }
        for (n, _) in data {
            require(self.view.superblock.layout.allocatable(*n), "data target")?;
        }
        let result = (|| {
            for (n, b) in data {
                self.device().write_at(n * 4096, b)?;
            }
            self.device().flush()?;
            for (ordinal, ((_, image), descriptor)) in images.iter().zip(&payload).enumerate() {
                self.device()
                    .write_at((3 + 2 * ordinal as u64) * 4096, descriptor)?;
                self.device()
                    .write_at((4 + 2 * ordinal as u64) * 4096, image)?;
            }
            self.device().flush()?;
            self.controls(JournalControl {
                sequence,
                committed: true,
                count: images.len() as u32,
                checksum,
            })?;
            for (n, b) in images {
                self.device().write_at(n * 4096, b)?;
            }
            self.device().flush()?;
            self.controls(JournalControl {
                sequence: clean_sequence,
                committed: false,
                count: 0,
                checksum: 0,
            })?;
            Ok(())
        })();
        if result.is_err() {
            self.faulted = true;
        } else {
            self.sequence = clean_sequence;
        }
        result
    }
    /// Set the executable flag and explicit atime/mtime, in whole seconds.
    pub fn set_attributes(
        &mut self,
        id: InodeId,
        executable: Option<bool>,
        atime: Option<u64>,
        mtime: Option<u64>,
    ) -> Result<()> {
        let mut inode = self.getattr(id)?;
        inode.times[2] = now();
        if let Some(x) = executable {
            if inode.kind != FileKind::File {
                return Err(FsError::IsDirectory);
            }
            inode.executable = x;
        }
        if let Some(t) = atime {
            inode.atime = t;
        }
        if let Some(t) = mtime {
            inode.times[1] = t;
        }
        let n = self.view.superblock.layout.table_start + id.index / 15;
        let mut block = self.view.metadata.block(n)?;
        let start = 64 + (id.index % 15) as usize * 256;
        block[start..start + 256].copy_from_slice(&inode.encode_revision(FormatRevision::Two)?);
        let block = with_revision(block, FormatRevision::Two);
        self.commit(&BTreeMap::from([(n, block)]), &[])?;
        self.view
            .nodes
            .get_mut(&id.index)
            .ok_or(FsError::Stale)?
            .inode = inode;
        Ok(())
    }
    pub fn sync(&mut self) -> Result<()> {
        self.healthy()?;
        if let Err(e) = self.device().flush() {
            self.faulted = true;
            return Err(e.into());
        }
        Ok(())
    }
    pub fn getattr(&mut self, id: InodeId) -> Result<Inode> {
        self.healthy()?;
        self.view.getattr(id)
    }
    pub fn lookup(&mut self, parent: InodeId, name: &[u8]) -> Result<InodeId> {
        self.healthy()?;
        self.view.lookup(parent, name)
    }
    pub fn inode_id(&self, index: u64) -> Result<InodeId> {
        self.healthy()?;
        self.view.inode_id(index)
    }
    pub fn read_file(&mut self, id: InodeId, offset: u64, length: usize) -> Result<Vec<u8>> {
        self.healthy()?;
        self.view.read_file(id, offset, length)
    }
    pub fn read_dir(&mut self, id: InodeId, cookie: u64, limit: usize) -> Result<DirectoryPage> {
        self.healthy()?;
        self.view.read_dir(id, cookie, limit)
    }
    pub fn statfs(&self) -> Result<StatFs> {
        self.healthy()?;
        Ok(self.view.statfs())
    }
}

const STAGING_RESERVE: usize = 8 * 1024 * 1024;
#[derive(Default)]
struct Edit {
    images: BTreeMap<u64, Block>,
    data: Vec<(u64, Block)>,
    bits: BTreeMap<u64, bool>,
    nodes: BTreeMap<u64, Node>,
    freed: Vec<InodeId>,
    next: u64,
}
fn node_memory(node: &Node) -> usize {
    1024 + MAX_CHAIN * 8
        + node.extents.len() * 64
        + node
            .entries
            .iter()
            .map(|r| 512 + r.name.len() * 12)
            .sum::<usize>()
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64)
}
impl<D: BlockDevice> ReadWriteFs<D> {
    fn edit(&self, ids: &[InodeId]) -> Result<Edit> {
        self.healthy()?;
        let mut reserve = STAGING_RESERVE;
        for id in ids {
            reserve = reserve
                .checked_add(node_memory(self.view.node(*id)?))
                .ok_or(FsError::ResourceLimit)?;
        }
        if self
            .view
            .memory_used
            .checked_add(reserve)
            .is_none_or(|n| n > self.memory_limit)
        {
            return Err(FsError::ResourceLimit);
        }
        Ok(Edit {
            next: self.next_block,
            ..Edit::default()
        })
    }
    fn used(&self, n: u64) -> bool {
        self.view.bitmap[n as usize / 8] & (1 << (n % 8)) != 0
    }
    fn allocate(&self, edit: &mut Edit) -> Result<u64> {
        let l = &self.view.superblock.layout;
        let start = edit.next;
        loop {
            let n = edit.next;
            edit.next = if n + 1 == l.blocks - 1 {
                l.data_start
            } else {
                n + 1
            };
            // Do not reuse blocks freed by this transaction: old metadata still owns them.
            if !self.used(n) && !edit.bits.contains_key(&n) {
                edit.bits.insert(n, true);
                return Ok(n);
            }
            if edit.next == start {
                return Err(FsError::NoSpace);
            }
        }
    }
    fn image(&mut self, edit: &mut Edit, n: u64) -> Result<Block> {
        if let Some(b) = edit.images.get(&n) {
            return Ok(*b);
        }
        self.view.metadata.block(n)
    }
    fn table_slot(&mut self, edit: &mut Edit, id: InodeId, slot: &[u8; 256]) -> Result<()> {
        let n = self.view.superblock.layout.table_start + id.index / 15;
        let mut b = self.image(edit, n)?;
        let start = 64 + (id.index % 15) as usize * 256;
        b[start..start + 256].copy_from_slice(slot);
        edit.images.insert(n, with_revision(b, FormatRevision::Two));
        Ok(())
    }
    fn coalesce(node: &mut Node) {
        node.extents.sort_by_key(|e| e.logical);
        let mut extents: Vec<Extent> = Vec::new();
        for e in std::mem::take(&mut node.extents) {
            if let Some(last) = extents.last_mut()
                && last.logical + last.length == e.logical
                && last.physical + last.length == e.physical
            {
                last.length += e.length;
                continue;
            }
            extents.push(e);
        }
        node.extents = extents;
    }
    fn stage_node(&mut self, edit: &mut Edit, mut node: Node) -> Result<()> {
        if node.inode.kind == FileKind::Directory && node.inode.state == InodeState::Linked {
            while let Some(e) = node.extents.last_mut() {
                let n = e.physical + e.length - 1;
                if !decode_directory(&self.image(edit, n)?, n, node.inode.id)?.is_empty() {
                    break;
                }
                edit.images.remove(&n);
                edit.bits.insert(n, false);
                e.length -= 1;
                if e.length == 0 {
                    node.extents.pop();
                }
                node.inode.size -= 4096;
            }
        }
        node.extents.sort_by_key(|e| e.logical);
        if node.extents.len() > MAX_EXTENTS {
            return Err(FsError::TooBig);
        }
        let mut chain = if let Some(old) = self.view.nodes.get(&node.inode.id.index) {
            let inode = old.inode.clone();
            resolve_extents(&inode, &self.view.superblock.layout, |n| {
                self.view.metadata.block(n)
            })?
            .1
        } else {
            vec![]
        };
        let needed = node
            .extents
            .len()
            .saturating_sub(4)
            .div_ceil(EXTENTS_PER_BLOCK);
        while chain.len() > needed {
            edit.bits.insert(chain.pop().unwrap(), false);
        }
        while chain.len() < needed {
            chain.push(self.allocate(edit)?);
        }
        for (k, &n) in chain.iter().enumerate() {
            let start = 4 + k * EXTENTS_PER_BLOCK;
            let end = (start + EXTENTS_PER_BLOCK).min(node.extents.len());
            let b = with_revision(
                encode_extents(
                    n,
                    node.inode.id,
                    chain.get(k + 1).copied().unwrap_or(0),
                    &node.extents[start..end],
                )?,
                FormatRevision::Two,
            );
            if !self.used(n) || self.view.metadata.block(n)? != b {
                edit.images.insert(n, b);
            }
        }
        node.inode.extent_count = node.extents.len();
        node.inode.inline = node.extents.iter().take(4).copied().collect();
        node.inode.overflow = chain.first().copied().unwrap_or(0);
        node.inode.allocated =
            node.extents.iter().map(|e| e.length).sum::<u64>() + chain.len() as u64;
        // Validate changed mappings with the same checked codec used at open.
        resolve_extents(&node.inode, &self.view.superblock.layout.clone(), |n| {
            self.image(edit, n)
        })?;
        self.table_slot(
            edit,
            node.inode.id,
            &node.inode.encode_revision(FormatRevision::Two)?,
        )?;
        edit.nodes.insert(node.inode.id.index, node);
        Ok(())
    }
    fn finish(&mut self, mut edit: Edit) -> Result<()> {
        let mut memory = self.view.memory_used;
        for (&index, node) in &edit.nodes {
            let old = self.view.nodes.get(&index).map(node_memory).unwrap_or(0);
            memory = memory
                .saturating_sub(old)
                .checked_add(node_memory(node))
                .ok_or(FsError::ResourceLimit)?;
        }
        for id in &edit.freed {
            memory = memory.saturating_sub(node_memory(&self.view.nodes[&id.index]));
        }
        if memory
            .checked_add(STAGING_RESERVE)
            .is_none_or(|n| n > self.memory_limit)
        {
            return Err(FsError::ResourceLimit);
        }
        for (&n, &set) in &edit.bits {
            let target = 515 + n / (PAYLOAD as u64 * 8);
            let mut b = if let Some(b) = edit.images.get(&target) {
                *b
            } else {
                self.view.metadata.block(target)?
            };
            let bit = n % (PAYLOAD as u64 * 8);
            if set {
                b[64 + bit as usize / 8] |= 1 << (bit % 8);
            } else {
                b[64 + bit as usize / 8] &= !(1 << (bit % 8));
            }
            edit.images
                .insert(target, with_revision(b, FormatRevision::Two));
        }
        if edit.images.len() > 256 {
            return Err(FsError::TooBig);
        }
        self.commit(&edit.images, &edit.data)?;
        for (n, set) in edit.bits {
            let was = self.used(n);
            if set {
                self.view.bitmap[n as usize / 8] |= 1 << (n % 8);
            } else {
                self.view.bitmap[n as usize / 8] &= !(1 << (n % 8));
            }
            if set != was {
                if set {
                    self.view.stats.free_blocks -= 1;
                } else {
                    self.view.stats.free_blocks += 1;
                }
            }
        }
        for (index, node) in edit.nodes {
            if self.view.nodes.insert(index, node).is_none() {
                self.view.stats.free_inodes -= 1;
            }
        }
        for id in edit.freed {
            self.view.nodes.remove(&id.index);
            if id.generation != u64::MAX {
                self.view.stats.free_inodes += 1;
            }
        }
        self.next_block = edit.next;
        self.view.memory_used = memory;
        Ok(())
    }
    fn physical(node: &Node, logical: u64) -> Option<u64> {
        node.extents
            .iter()
            .find(|e| logical >= e.logical && logical < e.logical + e.length)
            .map(|e| e.physical + logical - e.logical)
    }
    fn zero_tail(&mut self, edit: &mut Edit, node: &Node) -> Result<()> {
        if !node.inode.size.is_multiple_of(4096)
            && let Some(n) = Self::physical(node, node.inode.size / 4096)
        {
            let mut b = self.view.metadata.device.block(n)?;
            b[(node.inode.size % 4096) as usize..].fill(0);
            edit.data.push((n, b));
        }
        Ok(())
    }
    /// At most 1 MiB per request. A short result counts only retired transactions.
    pub fn write_file(&mut self, id: InodeId, offset: u64, bytes: &[u8]) -> Result<usize> {
        self.healthy()?;
        if bytes.len() > MAX_READ {
            return Err(FsError::TooBig);
        }
        offset
            .checked_add(bytes.len() as u64)
            .ok_or(FsError::InvalidInput)?;
        if self.getattr(id)?.kind != FileKind::File {
            return Err(FsError::IsDirectory);
        }
        let mut completed = 0;
        while completed < bytes.len() {
            let pos = offset + completed as u64;
            let count = (bytes.len() - completed).min(BLOCK - (pos % 4096) as usize);
            let result = self.write_piece(id, pos, &bytes[completed..completed + count]);
            if let Err(e) = result {
                return if completed == 0 {
                    Err(e)
                } else {
                    Ok(completed)
                };
            }
            completed += count;
        }
        Ok(completed)
    }
    fn write_piece(&mut self, id: InodeId, offset: u64, bytes: &[u8]) -> Result<()> {
        let mut edit = self.edit(&[id])?;
        let mut node = self.view.node(id)?.clone();
        if offset + bytes.len() as u64 > node.inode.size {
            self.zero_tail(&mut edit, &node)?;
        }
        let logical = offset / 4096;
        let existing = Self::physical(&node, logical);
        let n = if let Some(n) = existing {
            n
        } else {
            let n = self.allocate(&mut edit)?;
            node.extents.push(Extent {
                logical,
                physical: n,
                length: 1,
            });
            n
        };
        let mut b = if let Some((_, b)) = edit.data.iter().find(|(p, _)| *p == n) {
            *b
        } else if existing.is_some() {
            self.view.metadata.device.block(n)?
        } else {
            [0; BLOCK]
        };
        let start = (offset % 4096) as usize;
        b[start..start + bytes.len()].copy_from_slice(bytes);
        edit.data.retain(|(p, _)| *p != n);
        edit.data.push((n, b));
        Self::coalesce(&mut node);
        node.inode.size = node.inode.size.max(offset + bytes.len() as u64);
        node.inode.times[1] = now();
        node.inode.times[2] = now();
        self.stage_node(&mut edit, node)?;
        self.finish(edit)
    }
    pub fn append(&mut self, id: InodeId, bytes: &[u8]) -> Result<usize> {
        let offset = self.getattr(id)?.size;
        self.write_file(id, offset, bytes)
    }
    pub fn truncate(&mut self, id: InodeId, size: u64) -> Result<()> {
        self.healthy()?;
        self.cleanup_counters(id)?;
        let mut edit = self.edit(&[id])?;
        let mut node = self.view.node(id)?.clone();
        if node.inode.kind != FileKind::File {
            return Err(FsError::IsDirectory);
        }
        if size == node.inode.size {
            return self.cleanup_file(id);
        }
        if size < node.inode.size {
            node.inode.cleanup_bound = node.inode.size;
        } else {
            self.zero_tail(&mut edit, &node)?;
        }
        node.inode.size = size;
        node.inode.times[1] = now();
        node.inode.times[2] = now();
        self.stage_node(&mut edit, node)?;
        self.finish(edit)?;
        self.cleanup_file(id)
    }
    fn cleanup_file(&mut self, id: InodeId) -> Result<()> {
        while self.view.node(id)?.inode.cleanup_bound != 0 {
            let mut edit = self.edit(&[id])?;
            let mut node = self.view.node(id)?.clone();
            let keep = node.inode.size.div_ceil(4096);
            let mut remaining = 64;
            while remaining != 0 {
                let Some(e) = node.extents.last_mut() else {
                    break;
                };
                let excess = (e.logical + e.length).saturating_sub(keep.max(e.logical));
                if excess == 0 {
                    break;
                }
                let count = excess.min(remaining);
                for n in e.physical + e.length - count..e.physical + e.length {
                    edit.bits.insert(n, false);
                }
                e.length -= count;
                remaining -= count;
                if e.length == 0 {
                    node.extents.pop();
                }
            }
            if node
                .extents
                .last()
                .is_none_or(|e| e.logical + e.length <= keep)
            {
                node.inode.cleanup_bound = 0;
            }
            self.stage_node(&mut edit, node)?;
            self.finish(edit)?;
        }
        Ok(())
    }
    fn reclaim(&mut self, id: InodeId) -> Result<()> {
        if self.view.node(id)?.inode.kind == FileKind::File {
            self.truncate(id, 0)?;
        } else {
            while !self.view.node(id)?.extents.is_empty() {
                let mut edit = self.edit(&[id])?;
                let mut node = self.view.node(id)?.clone();
                require(node.entries.is_empty(), "reclaim nonempty directory")?;
                let e = node.extents.last_mut().unwrap();
                e.length -= 1;
                edit.bits.insert(e.physical + e.length, false);
                if e.length == 0 {
                    node.extents.pop();
                }
                node.inode.size -= 4096;
                self.stage_node(&mut edit, node)?;
                self.finish(edit)?;
            }
        }
        let mut edit = self.edit(&[id])?;
        self.table_slot(&mut edit, id, &free_inode(id)?)?;
        edit.freed.push(id);
        self.finish(edit)
    }
    fn resume_cleanup(&mut self) -> Result<()> {
        let ids: Vec<_> = self
            .view
            .nodes
            .values()
            .filter(|n| n.inode.state != InodeState::Linked || n.inode.cleanup_bound != 0)
            .map(|n| n.inode.id)
            .collect();
        for id in ids {
            if self.view.node(id)?.inode.state != InodeState::Linked {
                self.reclaim(id)?;
            } else {
                self.cleanup_file(id)?;
            }
        }
        Ok(())
    }
}

impl<D: BlockDevice> ReadWriteFs<D> {
    fn new_inode(&mut self, kind: FileKind, parent: InodeId) -> Result<Node> {
        let l = self.view.superblock.layout.clone();
        for table in 0..l.table_blocks {
            let b = self.view.metadata.block(l.table_start + table)?;
            for slot in 0..15 {
                let index = table * 15 + slot;
                if index >= l.inodes {
                    break;
                }
                let raw = &b[64 + slot as usize * 256..64 + (slot as usize + 1) * 256];
                if let Some(id) = next_inode_id(raw, index)? {
                    return Ok(Node {
                        inode: Inode {
                            id,
                            kind,
                            state: InodeState::Linked,
                            executable: false,
                            size: 0,
                            allocated: 0,
                            parent,
                            times: [now(); 3],
                            overflow: 0,
                            extent_count: 0,
                            inline: vec![],
                            cleanup_bound: 0,
                            atime: now(),
                        },
                        extents: vec![],
                        entries: vec![],
                    });
                }
            }
        }
        Err(FsError::NoInodes)
    }
    fn directory(&self, id: InodeId) -> Result<&Node> {
        let node = self.view.node(id)?;
        if node.inode.kind != FileKind::Directory {
            return Err(FsError::NotDirectory);
        }
        if node.inode.state != InodeState::Linked {
            return Err(FsError::NotFound);
        }
        Ok(node)
    }
    fn find_name(&self, id: InodeId, name: &[u8]) -> Result<Option<InodeId>> {
        let key = crate::names::comparison_key(name)?;
        for r in &self.directory(id)?.entries {
            if crate::names::comparison_key(r.name.as_bytes())? == key {
                return Ok(Some(r.child));
            }
        }
        Ok(None)
    }
    fn directory_image(
        edit: &mut Edit,
        n: u64,
        id: InodeId,
        records: &[DirectoryRecord],
    ) -> Result<()> {
        let mut payload = Vec::new();
        for r in records {
            payload.extend(r.encode()?);
        }
        edit.images.insert(
            n,
            with_revision(
                encode_block(Kind::Directory, n, id, &payload)?,
                FormatRevision::Two,
            ),
        );
        if edit.images.len() > 256 {
            return Err(FsError::TooBig);
        }
        Ok(())
    }
    fn insert_record(
        &mut self,
        edit: &mut Edit,
        node: &mut Node,
        record: DirectoryRecord,
    ) -> Result<()> {
        let needed = record.encode()?.len();
        let mut position = 0;
        for e in &node.extents {
            for n in e.physical..e.physical + e.length {
                let b = self.image(edit, n)?;
                let mut records = decode_directory(&b, n, node.inode.id)?;
                position += records.len();
                if decode_block(&b, n)?.used + needed <= PAYLOAD {
                    records.push(record.clone());
                    Self::directory_image(edit, n, node.inode.id, &records)?;
                    node.entries.insert(position, record);
                    node.inode.times[1] = now();
                    node.inode.times[2] = now();
                    return Ok(());
                }
            }
        }
        let n = self.allocate(edit)?;
        Self::directory_image(edit, n, node.inode.id, std::slice::from_ref(&record))?;
        node.extents.push(Extent {
            logical: node.inode.size / 4096,
            physical: n,
            length: 1,
        });
        Self::coalesce(node);
        node.inode.size += 4096;
        node.entries.push(record);
        node.inode.times[1] = now();
        node.inode.times[2] = now();
        Ok(())
    }
    fn remove_record(&mut self, edit: &mut Edit, node: &mut Node, id: InodeId) -> Result<()> {
        let mut found = false;
        for e in &node.extents {
            for n in e.physical..e.physical + e.length {
                let mut records = decode_directory(&self.image(edit, n)?, n, node.inode.id)?;
                if let Some(pos) = records.iter().position(|r| r.child == id) {
                    records.remove(pos);
                    Self::directory_image(edit, n, node.inode.id, &records)?;
                    found = true;
                    break;
                }
            }
            if found {
                break;
            }
        }
        require(found, "missing directory record")?;
        node.entries.retain(|r| r.child != id);
        node.inode.times[1] = now();
        node.inode.times[2] = now();
        Ok(())
    }
    pub fn create(&mut self, parent: InodeId, name: &[u8], executable: bool) -> Result<InodeId> {
        self.create_kind(parent, name, FileKind::File, executable)
    }
    pub fn mkdir(&mut self, parent: InodeId, name: &[u8]) -> Result<InodeId> {
        self.create_kind(parent, name, FileKind::Directory, false)
    }
    fn create_kind(
        &mut self,
        parent: InodeId,
        name: &[u8],
        kind: FileKind,
        executable: bool,
    ) -> Result<InodeId> {
        let mut edit = self.edit(&[parent])?;
        if self.find_name(parent, name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        let mut node = self.new_inode(kind, parent)?;
        node.inode.executable = executable;
        let id = node.inode.id;
        let mut directory = self.directory(parent)?.clone();
        let record = DirectoryRecord {
            name: crate::names::validate_name(name)?.into(),
            child: id,
        };
        self.insert_record(&mut edit, &mut directory, record)?;
        self.stage_node(&mut edit, node)?;
        self.stage_node(&mut edit, directory)?;
        self.finish(edit)?;
        Ok(id)
    }
    /// Retain an identity independently of its namespace entry.
    pub fn open_handle(&mut self, id: InodeId) -> Result<()> {
        self.healthy()?;
        self.view.node(id)?;
        if self.opens.values().sum::<u64>() >= 65536 {
            return Err(FsError::ResourceLimit);
        }
        if !self.opens.contains_key(&id) {
            if self.view.memory_used + 128 + STAGING_RESERVE > self.memory_limit {
                return Err(FsError::ResourceLimit);
            }
            self.view.memory_used += 128;
        }
        *self.opens.entry(id).or_default() += 1;
        Ok(())
    }
    /// Disposal remains possible even after a backend failure faults the writer.
    pub fn close_handle(&mut self, id: InodeId) -> Result<()> {
        let count = self.opens.get_mut(&id).ok_or(FsError::Stale)?;
        *count -= 1;
        if *count != 0 {
            return Ok(());
        }
        self.opens.remove(&id);
        self.view.memory_used -= 128;
        if !self.faulted && self.view.node(id)?.inode.state != InodeState::Linked {
            self.reclaim(id)?;
        }
        Ok(())
    }
    fn cleanup_counters(&self, id: InodeId) -> Result<()> {
        let i = &self.view.node(id)?.inode;
        let steps = if i.kind == FileKind::Directory {
            i.allocated
        } else {
            i.allocated.div_ceil(64)
        };
        self.sequence
            .checked_add(
                steps
                    .checked_add(4)
                    .and_then(|s| s.checked_mul(2))
                    .ok_or(FsError::CounterExhausted)?,
            )
            .ok_or(FsError::CounterExhausted)?;
        Ok(())
    }
    fn detach(&mut self, edit: &mut Edit, id: InodeId) -> Result<()> {
        self.cleanup_counters(id)?;
        let mut node = self.view.node(id)?.clone();
        node.inode.state = InodeState::Orphan;
        node.inode.parent = InodeId::default();
        node.inode.times[2] = now();
        self.stage_node(edit, node)
    }
    fn finish_detached(&mut self, id: InodeId) -> Result<()> {
        if !self.opens.contains_key(&id) {
            self.reclaim(id)?;
        }
        Ok(())
    }
    pub fn unlink(&mut self, parent: InodeId, name: &[u8]) -> Result<()> {
        self.remove_name(parent, name, false)
    }
    pub fn rmdir(&mut self, parent: InodeId, name: &[u8]) -> Result<()> {
        self.remove_name(parent, name, true)
    }
    fn remove_name(&mut self, parent: InodeId, name: &[u8], directory: bool) -> Result<()> {
        self.healthy()?;
        let id = self.find_name(parent, name)?.ok_or(FsError::NotFound)?;
        let node = self.view.node(id)?;
        if directory && node.inode.kind != FileKind::Directory {
            return Err(FsError::NotDirectory);
        }
        if !directory && node.inode.kind == FileKind::Directory {
            return Err(FsError::IsDirectory);
        }
        if !node.entries.is_empty() {
            return Err(FsError::NotEmpty);
        }
        let mut edit = self.edit(&[parent, id])?;
        let mut parent_node = self.directory(parent)?.clone();
        self.remove_record(&mut edit, &mut parent_node, id)?;
        self.stage_node(&mut edit, parent_node)?;
        self.detach(&mut edit, id)?;
        self.finish(edit)?;
        self.finish_detached(id)
    }
    pub fn rename(
        &mut self,
        parent: InodeId,
        name: &[u8],
        new_parent: InodeId,
        new_name: &[u8],
        no_replace: bool,
    ) -> Result<()> {
        self.healthy()?;
        let id = self.find_name(parent, name)?.ok_or(FsError::NotFound)?;
        let target = self.find_name(new_parent, new_name)?;
        if no_replace && target.is_some() {
            return Err(FsError::AlreadyExists);
        }
        let mut ids = vec![parent, new_parent, id];
        if let Some(t) = target {
            ids.push(t);
        }
        let mut edit = self.edit(&ids)?;
        let mut moved = self.view.node(id)?.clone();
        if moved.inode.kind == FileKind::Directory {
            let mut ancestor = new_parent;
            loop {
                if ancestor == id {
                    return Err(FsError::InvalidInput);
                }
                if ancestor == ROOT {
                    break;
                }
                ancestor = self.directory(ancestor)?.inode.parent;
            }
        }
        let replacement = target.filter(|t| *t != id);
        if let Some(t) = replacement {
            let n = self.view.node(t)?;
            if n.inode.kind != moved.inode.kind {
                return Err(if n.inode.kind == FileKind::Directory {
                    FsError::IsDirectory
                } else {
                    FsError::NotDirectory
                });
            }
            if !n.entries.is_empty() {
                return Err(FsError::NotEmpty);
            }
            self.detach(&mut edit, t)?;
        }
        let record = DirectoryRecord {
            name: crate::names::validate_name(new_name)?.into(),
            child: id,
        };
        let mut old = self.directory(parent)?.clone();
        self.remove_record(&mut edit, &mut old, id)?;
        if parent == new_parent {
            if let Some(t) = replacement {
                self.remove_record(&mut edit, &mut old, t)?;
            }
            self.insert_record(&mut edit, &mut old, record)?;
            self.stage_node(&mut edit, old)?;
        } else {
            let mut new = self.directory(new_parent)?.clone();
            if let Some(t) = replacement {
                self.remove_record(&mut edit, &mut new, t)?;
            }
            self.insert_record(&mut edit, &mut new, record)?;
            self.stage_node(&mut edit, old)?;
            self.stage_node(&mut edit, new)?;
        }
        moved.inode.parent = new_parent;
        moved.inode.times[2] = now();
        self.stage_node(&mut edit, moved)?;
        self.finish(edit)?;
        if let Some(t) = replacement {
            self.finish_detached(t)?;
        }
        Ok(())
    }
}
