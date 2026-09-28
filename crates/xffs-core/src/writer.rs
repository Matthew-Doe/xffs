//! Serialized durable image writer. No device I/O is exposed to callers.
use crate::{
    AccessMode, BlockDevice, ImageDevice, ReadOnlyFs,
    format::*,
    reader::{DirectoryPage, OpenOptions, StatFs},
};
use std::{collections::BTreeMap, path::Path};

pub struct ReadWriteFs<D: BlockDevice = ImageDevice> {
    pub(crate) view: ReadOnlyFs<D>,
    sequence: u64,
    faulted: bool,
    memory_limit: usize,
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
        let mut fs = Self {
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
