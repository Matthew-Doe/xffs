//! Explicit checked codecs for docs/on-disk-format.md. Contextual ownership is separate.
use crate::DeviceError;
use std::fmt;
pub const BLOCK: usize = 4096;
pub const PAYLOAD: usize = 4032;
pub const MAX_EXTENTS: usize = 65536;
pub const EXTENTS_PER_BLOCK: usize = 167;
pub const MAX_CHAIN: usize = (MAX_EXTENTS - 4).div_ceil(EXTENTS_PER_BLOCK);
pub type Block = [u8; BLOCK];
pub type Result<T> = std::result::Result<T, FsError>;
#[derive(Debug)]
pub enum FsError {
    Device(DeviceError),
    Corrupt(&'static str),
    Unsupported,
    ResourceLimit,
    InvalidName,
    NotFound,
    NotDirectory,
    IsDirectory,
    Stale,
}
impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(e) => write!(f, "{e}"),
            x => write!(f, "XFFS: {x:?}"),
        }
    }
}
impl std::error::Error for FsError {}
impl From<DeviceError> for FsError {
    fn from(e: DeviceError) -> Self {
        Self::Device(e)
    }
}
pub(crate) fn require(ok: bool, reason: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(FsError::Corrupt(reason))
    }
}
fn zero(b: &[u8]) -> Result<()> {
    require(b.iter().all(|&x| x == 0), "nonzero reserved bytes")
}
fn get<const N: usize>(b: &[u8], o: usize) -> Result<[u8; N]> {
    b.get(
        o..o.checked_add(N)
            .ok_or(FsError::Corrupt("offset overflow"))?,
    )
    .ok_or(FsError::Corrupt("truncated field"))?
    .try_into()
    .map_err(|_| FsError::Corrupt("field"))
}
fn u16at(b: &[u8], o: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(get(b, o)?))
}
fn u32at(b: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(get(b, o)?))
}
fn u64at(b: &[u8], o: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(get(b, o)?))
}
fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatVersion {
    pub major: u16,
    pub minor: u16,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InodeId {
    pub index: u64,
    pub generation: u64,
}
pub const ROOT: InodeId = InodeId {
    index: 0,
    generation: 1,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Kind {
    Super = 1,
    Control = 2,
    Descriptor = 3,
    Bitmap = 4,
    Inodes = 5,
    Directory = 6,
    Extents = 7,
}
impl Kind {
    fn decode(v: u16) -> Result<Self> {
        Ok(match v {
            1 => Self::Super,
            2 => Self::Control,
            3 => Self::Descriptor,
            4 => Self::Bitmap,
            5 => Self::Inodes,
            6 => Self::Directory,
            7 => Self::Extents,
            _ => return Err(FsError::Corrupt("block type")),
        })
    }
}
#[derive(Debug)]
pub struct Header {
    pub kind: Kind,
    pub block: u64,
    pub owner: InodeId,
    pub used: usize,
}
pub fn decode_block(b: &[u8], expected: u64) -> Result<Header> {
    require(b.len() == BLOCK, "block length")?;
    require(&b[..8] == b"XFFSMETA", "magic")?;
    let mut copy = [0; BLOCK];
    copy.copy_from_slice(b);
    copy[40..44].fill(0);
    require(crc32c::crc32c(&copy) == u32at(b, 40)?, "block checksum")?;
    if u16at(b, 10)? != 1 || u16at(b, 12)? != 0 {
        return Err(FsError::Unsupported);
    }
    zero(&b[14..16])?;
    zero(&b[48..64])?;
    let h = Header {
        kind: Kind::decode(u16at(b, 8)?)?,
        block: u64at(b, 16)?,
        owner: InodeId {
            index: u64at(b, 24)?,
            generation: u64at(b, 32)?,
        },
        used: u32at(b, 44)? as usize,
    };
    require(
        h.block == expected && h.used <= PAYLOAD,
        "block identity or size",
    )?;
    zero(&b[64 + h.used..])?;
    if !matches!(h.kind, Kind::Directory | Kind::Extents) {
        require(h.owner == InodeId::default(), "unexpected owner")?;
    }
    Ok(h)
}
pub fn encode_block(kind: Kind, block: u64, owner: InodeId, payload: &[u8]) -> Result<Block> {
    require(payload.len() <= PAYLOAD, "payload length")?;
    let mut b = [0; BLOCK];
    b[..8].copy_from_slice(b"XFFSMETA");
    put16(&mut b, 8, kind as u16);
    put16(&mut b, 10, 1);
    put64(&mut b, 16, block);
    put64(&mut b, 24, owner.index);
    put64(&mut b, 32, owner.generation);
    put32(&mut b, 44, payload.len() as u32);
    b[64..64 + payload.len()].copy_from_slice(payload);
    let crc = crc32c::crc32c(&b);
    put32(&mut b, 40, crc);
    Ok(b)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeLayout {
    pub blocks: u64,
    pub inodes: u64,
    pub bitmap_blocks: u64,
    pub table_start: u64,
    pub table_blocks: u64,
    pub data_start: u64,
}
impl VolumeLayout {
    pub fn new(bytes: u64, inodes: Option<u64>) -> Result<Self> {
        let blocks = bytes / 4096;
        let inodes = inodes.unwrap_or((blocks / 16).max(256));
        require(blocks >= 4096 && inodes >= 256, "minimum geometry")?;
        let bitmap_blocks = blocks.div_ceil(32256);
        let table_start = 515 + bitmap_blocks;
        let table_blocks = inodes.div_ceil(15);
        let data_start = table_start
            .checked_add(table_blocks)
            .ok_or(FsError::Corrupt("layout overflow"))?;
        require(data_start < blocks - 1, "inode table does not fit")?;
        Ok(Self {
            blocks,
            inodes,
            bitmap_blocks,
            table_start,
            table_blocks,
            data_start,
        })
    }
    pub fn allocatable(&self, b: u64) -> bool {
        b >= self.data_start && b < self.blocks - 1
    }
    pub fn metadata_target(&self, b: u64, kind: Kind) -> bool {
        match kind {
            Kind::Bitmap => (515..self.table_start).contains(&b),
            Kind::Inodes => (self.table_start..self.data_start).contains(&b),
            Kind::Directory | Kind::Extents => self.allocatable(b),
            _ => false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Superblock {
    pub uuid: [u8; 16],
    pub layout: VolumeLayout,
}
impl Superblock {
    pub fn encode(&self, block: u64) -> Result<Block> {
        let l = &self.layout;
        let mut p = [0; 80];
        p[..16].copy_from_slice(&self.uuid);
        for (o, v) in [
            (16, l.blocks),
            (24, l.inodes),
            (32, l.bitmap_blocks),
            (40, l.table_start),
            (48, l.table_blocks),
            (56, l.data_start),
        ] {
            put64(&mut p, o, v);
        }
        put32(&mut p, 64, 1);
        encode_block(Kind::Super, block, InodeId::default(), &p)
    }
    pub fn decode(b: &[u8], block: u64, bytes: u64) -> Result<Self> {
        let h = decode_block(b, block)?;
        require(
            h.kind == Kind::Super && h.used == 80,
            "superblock type/size",
        )?;
        if u32at(b, 128)? != 1 || b[132..144].iter().any(|&x| x != 0) {
            return Err(FsError::Unsupported);
        }
        let l = VolumeLayout::new(bytes, Some(u64at(b, 88)?))?;
        require(
            u64at(b, 80)? == l.blocks
                && u64at(b, 96)? == l.bitmap_blocks
                && u64at(b, 104)? == l.table_start
                && u64at(b, 112)? == l.table_blocks
                && u64at(b, 120)? == l.data_start,
            "superblock layout",
        )?;
        require(block == 0 || block == l.blocks - 1, "superblock location")?;
        Ok(Self {
            uuid: get(b, 64)?,
            layout: l,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub logical: u64,
    pub physical: u64,
    pub length: u64,
}
impl Extent {
    fn decode(b: &[u8]) -> Result<Self> {
        let e = Self {
            logical: u64at(b, 0)?,
            physical: u64at(b, 8)?,
            length: u64at(b, 16)?,
        };
        require(
            e.length > 0
                && e.logical.checked_add(e.length).is_some()
                && e.physical.checked_add(e.length).is_some(),
            "extent overflow/length",
        )?;
        Ok(e)
    }
    fn encode(self, b: &mut [u8]) {
        put64(b, 0, self.logical);
        put64(b, 8, self.physical);
        put64(b, 16, self.length);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InodeState {
    Linked,
    Orphan,
    Pending,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inode {
    pub id: InodeId,
    pub kind: FileKind,
    pub state: InodeState,
    pub executable: bool,
    pub size: u64,
    pub allocated: u64,
    pub parent: InodeId,
    pub times: [u64; 3],
    pub overflow: u64,
    pub extent_count: usize,
    pub inline: Vec<Extent>,
}
impl Inode {
    pub fn decode(b: &[u8], index: u64) -> Result<Option<Self>> {
        require(b.len() == 256, "inode length")?;
        if b.iter().all(|&x| x == 0) {
            return Ok(None);
        }
        let id = InodeId {
            index: u64at(b, 0)?,
            generation: u64at(b, 8)?,
        };
        require(id.index == index && id.generation != 0, "inode identity")?;
        let kind = match b[16] {
            1 => FileKind::File,
            2 => FileKind::Directory,
            _ => return Err(FsError::Corrupt("inode kind")),
        };
        let state = match b[17] {
            1 => InodeState::Linked,
            2 => InodeState::Orphan,
            3 => InodeState::Pending,
            _ => return Err(FsError::Corrupt("inode state")),
        };
        require(
            b[18] <= 1 && (kind == FileKind::File || b[18] == 0),
            "executable flag",
        )?;
        zero(&b[19..24])?;
        zero(&b[92..96])?;
        zero(&b[192..])?;
        let extent_count = u32at(b, 88)? as usize;
        require(extent_count <= MAX_EXTENTS, "extent limit")?;
        let overflow = u64at(b, 80)?;
        require((extent_count > 4) == (overflow != 0), "overflow head")?;
        let mut inline = Vec::new();
        for n in 0..extent_count.min(4) {
            inline.push(Extent::decode(&b[96 + n * 24..120 + n * 24])?);
        }
        zero(&b[96 + inline.len() * 24..192])?;
        let times = [u64at(b, 56)?, u64at(b, 64)?, u64at(b, 72)?];
        require(times.iter().all(|&t| t <= i64::MAX as u64), "timestamp")?;
        let parent = InodeId {
            index: u64at(b, 40)?,
            generation: u64at(b, 48)?,
        };
        require(
            state == InodeState::Linked || (parent == InodeId::default() && kind == FileKind::File),
            "detached inode",
        )?;
        Ok(Some(Self {
            id,
            kind,
            state,
            executable: b[18] == 1,
            size: u64at(b, 24)?,
            allocated: u64at(b, 32)?,
            parent,
            times,
            overflow,
            extent_count,
            inline,
        }))
    }
    pub fn encode(&self) -> Result<[u8; 256]> {
        require(
            self.inline.len() == self.extent_count.min(4),
            "inline count",
        )?;
        let mut b = [0; 256];
        put64(&mut b, 0, self.id.index);
        put64(&mut b, 8, self.id.generation);
        b[16] = if self.kind == FileKind::File { 1 } else { 2 };
        b[17] = match self.state {
            InodeState::Linked => 1,
            InodeState::Orphan => 2,
            InodeState::Pending => 3,
        };
        b[18] = u8::from(self.executable);
        for (o, v) in [
            (24, self.size),
            (32, self.allocated),
            (40, self.parent.index),
            (48, self.parent.generation),
            (56, self.times[0]),
            (64, self.times[1]),
            (72, self.times[2]),
            (80, self.overflow),
        ] {
            put64(&mut b, o, v);
        }
        require(self.extent_count <= MAX_EXTENTS, "extent count")?;
        put32(&mut b, 88, self.extent_count as u32);
        for (n, e) in self.inline.iter().enumerate() {
            e.encode(&mut b[96 + n * 24..120 + n * 24]);
        }
        Self::decode(&b, self.id.index)?;
        Ok(b)
    }
}
pub fn decode_extents(b: &[u8], block: u64, owner: InodeId) -> Result<(u64, Vec<Extent>)> {
    let h = decode_block(b, block)?;
    require(
        h.kind == Kind::Extents && h.owner == owner && h.used >= 16,
        "extent block identity",
    )?;
    let count = u32at(b, 72)? as usize;
    require(
        count > 0 && count <= EXTENTS_PER_BLOCK && h.used == 16 + count * 24,
        "extent block count",
    )?;
    zero(&b[76..80])?;
    let mut v = Vec::with_capacity(count);
    for n in 0..count {
        v.push(Extent::decode(&b[80 + n * 24..104 + n * 24])?);
    }
    Ok((u64at(b, 64)?, v))
}
pub fn encode_extents(block: u64, owner: InodeId, next: u64, extents: &[Extent]) -> Result<Block> {
    require(
        !extents.is_empty() && extents.len() <= EXTENTS_PER_BLOCK,
        "extent block count",
    )?;
    let mut p = vec![0; 16 + extents.len() * 24];
    put64(&mut p, 0, next);
    put32(&mut p, 8, extents.len() as u32);
    for (n, e) in extents.iter().enumerate() {
        e.encode(&mut p[16 + n * 24..40 + n * 24]);
    }
    encode_block(Kind::Extents, block, owner, &p)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryRecord {
    pub name: String,
    pub child: InodeId,
}
impl DirectoryRecord {
    pub fn encode(&self) -> Result<Vec<u8>> {
        crate::names::validate_name(self.name.as_bytes())?;
        require(self.child.generation > 0, "directory generation")?;
        let len = (24 + self.name.len()).next_multiple_of(8);
        let mut b = vec![0; len];
        put16(&mut b, 0, len as u16);
        put16(&mut b, 2, self.name.len() as u16);
        put64(&mut b, 8, self.child.index);
        put64(&mut b, 16, self.child.generation);
        b[24..24 + self.name.len()].copy_from_slice(self.name.as_bytes());
        Ok(b)
    }
}
pub fn decode_directory(b: &[u8], block: u64, owner: InodeId) -> Result<Vec<DirectoryRecord>> {
    let h = decode_block(b, block)?;
    require(
        h.kind == Kind::Directory && h.owner == owner,
        "directory identity",
    )?;
    let mut out = Vec::new();
    let mut o = 64;
    while o < 64 + h.used {
        let tail = &b[o..64 + h.used];
        let len = u16at(tail, 0)? as usize;
        let n = u16at(tail, 2)? as usize;
        require(
            n <= 255 && len == (24 + n).next_multiple_of(8) && len <= tail.len(),
            "directory record length",
        )?;
        zero(&tail[4..8])?;
        zero(&tail[24 + n..len])?;
        let name = crate::names::validate_name(&tail[24..24 + n])
            .map_err(|_| FsError::Corrupt("invalid directory name"))?
            .to_owned();
        let child = InodeId {
            index: u64at(tail, 8)?,
            generation: u64at(tail, 16)?,
        };
        require(child.generation > 0, "directory generation")?;
        out.push(DirectoryRecord { name, child });
        o += len;
    }
    Ok(out)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalControl {
    pub sequence: u64,
    pub committed: bool,
    pub count: u32,
    pub checksum: u32,
}
impl JournalControl {
    pub fn decode(b: &[u8], block: u64) -> Result<Self> {
        let h = decode_block(b, block)?;
        require(
            h.kind == Kind::Control && h.used == 24 && (block == 1 || block == 2),
            "control identity",
        )?;
        zero(&b[84..88])?;
        let state = u32at(b, 72)?;
        let c = Self {
            sequence: u64at(b, 64)?,
            committed: state == 1,
            count: u32at(b, 76)?,
            checksum: u32at(b, 80)?,
        };
        require(
            c.sequence > 0
                && state <= 1
                && if c.committed {
                    (1..=256).contains(&c.count)
                } else {
                    c.count == 0 && c.checksum == 0
                },
            "control fields",
        )?;
        Ok(c)
    }
    pub fn encode(self, block: u64) -> Result<Block> {
        let mut p = [0; 24];
        put64(&mut p, 0, self.sequence);
        put32(&mut p, 8, u32::from(self.committed));
        put32(&mut p, 12, self.count);
        put32(&mut p, 16, self.checksum);
        let b = encode_block(Kind::Control, block, InodeId::default(), &p)?;
        Self::decode(&b, block)?;
        Ok(b)
    }
}
pub fn select_control(
    a: Result<JournalControl>,
    b: Result<JournalControl>,
) -> Result<(JournalControl, bool)> {
    match (a, b) {
        (Err(FsError::Unsupported), _) | (_, Err(FsError::Unsupported)) => {
            Err(FsError::Unsupported)
        }
        (Ok(a), Ok(b)) => {
            if a.sequence == b.sequence {
                require(a == b, "conflicting controls")?;
                Ok((a, false))
            } else {
                let (old, new) = if a.sequence < b.sequence {
                    (a, b)
                } else {
                    (b, a)
                };
                require(
                    new.sequence - old.sequence == 1 && new.committed != old.committed,
                    "control transition",
                )?;
                Ok((new, false))
            }
        }
        (Ok(c), Err(_)) | (Err(_), Ok(c)) => Ok((c, true)),
        _ => Err(FsError::Corrupt("no valid journal control")),
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalDescriptor {
    pub target: u64,
    pub ordinal: u32,
    pub image_checksum: u32,
    pub sequence: u64,
}
impl JournalDescriptor {
    pub fn decode(b: &[u8], ordinal: u32) -> Result<Self> {
        require(ordinal < 256, "descriptor ordinal")?;
        let h = decode_block(b, 3 + 2 * u64::from(ordinal))?;
        require(
            h.kind == Kind::Descriptor && h.used == 24,
            "descriptor identity",
        )?;
        let d = Self {
            target: u64at(b, 64)?,
            ordinal: u32at(b, 72)?,
            image_checksum: u32at(b, 76)?,
            sequence: u64at(b, 80)?,
        };
        require(d.ordinal == ordinal && d.sequence > 0, "descriptor fields")?;
        Ok(d)
    }
    pub fn encode(self) -> Result<Block> {
        require(self.ordinal < 256, "descriptor ordinal")?;
        let mut p = [0; 24];
        put64(&mut p, 0, self.target);
        put32(&mut p, 8, self.ordinal);
        put32(&mut p, 12, self.image_checksum);
        put64(&mut p, 16, self.sequence);
        encode_block(
            Kind::Descriptor,
            3 + 2 * u64::from(self.ordinal),
            InodeId::default(),
            &p,
        )
    }
}

/// Resolve a bounded chain; allocation and cross-inode ownership remain caller checks.
pub fn resolve_extents(
    inode: &Inode,
    layout: &VolumeLayout,
    mut read: impl FnMut(u64) -> Result<Block>,
) -> Result<(Vec<Extent>, Vec<u64>)> {
    require(
        inode.extent_count <= MAX_EXTENTS && inode.inline.len() == inode.extent_count.min(4),
        "extent count",
    )?;
    let mut extents = inode.inline.clone();
    let mut chain = Vec::new();
    let mut next = inode.overflow;
    while next != 0 {
        require(
            chain.len() < MAX_CHAIN && layout.allocatable(next) && !chain.contains(&next),
            "invalid extent chain",
        )?;
        chain.push(next);
        let b = read(next)?;
        let (following, items) = decode_extents(&b, next, inode.id)?;
        require(
            items.len() == (inode.extent_count - extents.len()).min(EXTENTS_PER_BLOCK),
            "extent packing",
        )?;
        extents.extend(items);
        next = following;
        require(
            extents.len() < inode.extent_count || next == 0,
            "excess chain",
        )?;
    }
    require(extents.len() == inode.extent_count, "short extent chain")?;
    let mut end = 0;
    let mut allocated = chain.len() as u64;
    for e in &extents {
        let logical_end = e
            .logical
            .checked_add(e.length)
            .ok_or(FsError::Corrupt("extent overflow"))?;
        let physical_end = e
            .physical
            .checked_add(e.length)
            .ok_or(FsError::Corrupt("extent overflow"))?;
        require(
            e.length > 0
                && e.logical >= end
                && logical_end <= inode.size.div_ceil(4096)
                && layout.allocatable(e.physical)
                && physical_end < layout.blocks,
            "invalid extent mapping",
        )?;
        if inode.kind == FileKind::Directory {
            require(e.logical == end, "directory hole")?;
        }
        end = logical_end;
        allocated = allocated
            .checked_add(e.length)
            .ok_or(FsError::Corrupt("allocated overflow"))?;
    }
    require(allocated == inode.allocated, "allocation accounting")?;
    if inode.kind == FileKind::Directory {
        require(
            inode.size.is_multiple_of(4096) && end == inode.size / 4096,
            "directory size",
        )?;
    }
    Ok((extents, chain))
}
