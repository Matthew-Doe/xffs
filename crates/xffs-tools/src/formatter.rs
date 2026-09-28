//! Streaming empty-filesystem formatter. No allocation proportional to capacity.
use crate::{Builder, ToolResult};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
};
use xffs_core::{BlockDevice, DeviceError, Operation, format::*};

/// All fallible layout/encoding validation precedes the first write. On failure
/// after mutation, the caller must report interruption, never restore old data.
pub fn format_empty<D: BlockDevice>(
    device: &mut D,
    uuid: [u8; 16],
    inodes: Option<u64>,
    revision: FormatRevision,
) -> ToolResult<VolumeLayout> {
    let layout = VolumeLayout::new(device.capacity_bytes(), inodes)?;
    let mut builder = Builder::new(layout.clone());
    builder.inode(FileKind::Directory, ROOT)?;
    let sb = Superblock {
        revision,
        uuid,
        layout: layout.clone(),
    };
    let primary = sb.encode(0)?;
    let backup = sb.encode(layout.blocks - 1)?;
    let root = with_revision(builder.table(0)?, revision);
    let control = JournalControl {
        sequence: 1,
        committed: false,
        count: 0,
        checksum: 0,
    };
    let controls = [
        with_revision(control.encode(1)?, revision),
        with_revision(control.encode(2)?, revision),
    ];
    let result = (|| -> ToolResult<()> {
        let zero = [0; BLOCK];
        device.write_at(0, &zero)?;
        device.write_at((layout.blocks - 1) * 4096, &zero)?;
        device.flush()?;
        // Erase conventional whole-disk MBR/GPT regions, including 4K-sector GPT.
        // Data elsewhere is not securely erased. Clear journal payload as well.
        for n in 1..515 {
            device.write_at(n * 4096, &zero)?;
        }
        for n in layout.blocks - 256..layout.blocks - 1 {
            device.write_at(n * 4096, &zero)?;
        }
        for (n, block) in controls.iter().enumerate() {
            device.write_at((n as u64 + 1) * 4096, block)?;
        }
        for n in 0..layout.bitmap_blocks {
            let mut p = [0; PAYLOAD];
            for bit in 0..32256 {
                let absolute = n * 32256 + bit;
                if absolute < layout.data_start || absolute == layout.blocks - 1 {
                    p[bit as usize / 8] |= 1 << (bit % 8);
                }
            }
            let block = with_revision(
                encode_block(Kind::Bitmap, 515 + n, InodeId::default(), &p)?,
                revision,
            );
            device.write_at((515 + n) * 4096, &block)?;
        }
        device.write_at(layout.table_start * 4096, &root)?;
        for n in 1..layout.table_blocks {
            let block = with_revision(builder.table(n)?, revision);
            device.write_at((layout.table_start + n) * 4096, &block)?;
        }
        device.flush()?;
        // Either published superblock refers only to fully initialized metadata.
        device.write_at(0, &primary)?;
        device.flush()?;
        device.write_at((layout.blocks - 1) * 4096, &backup)?;
        device.flush()?;
        Ok(())
    })();
    result.map_err(|error| format!("format interrupted; target may be incomplete: {error}"))?;
    Ok(layout)
}

/// Adapter for a newly created, exclusively locked image. Its creator owns
/// cleanup; the common formatter never truncates, resizes or removes anything.
pub(crate) struct NewImage<'a> {
    pub file: &'a mut File,
    pub capacity: u64,
}
impl NewImage<'_> {
    fn io<T>(
        result: std::io::Result<T>,
        operation: Operation,
    ) -> std::result::Result<T, DeviceError> {
        result.map_err(|source| DeviceError::Io {
            operation,
            offset: None,
            transferred: None,
            source,
        })
    }
}
impl BlockDevice for NewImage<'_> {
    fn capacity_bytes(&self) -> u64 {
        self.capacity
    }
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> std::result::Result<(), DeviceError> {
        xffs_core::validate_range(self.capacity, offset, out.len())?;
        Self::io(self.file.seek(SeekFrom::Start(offset)), Operation::Read)?;
        Self::io(self.file.read_exact(out), Operation::Read)
    }
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> std::result::Result<(), DeviceError> {
        xffs_core::validate_range(self.capacity, offset, bytes.len())?;
        Self::io(self.file.seek(SeekFrom::Start(offset)), Operation::Write)?;
        Self::io(self.file.write_all(bytes), Operation::Write)
    }
    fn flush(&mut self) -> std::result::Result<(), DeviceError> {
        Self::io(self.file.sync_all(), Operation::Flush)
    }
}
