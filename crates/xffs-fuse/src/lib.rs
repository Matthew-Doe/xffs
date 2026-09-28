//! Linux image FUSE adapter. All operations serialize through one state lock.
#![forbid(unsafe_code)]
use fuser::*;
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::Path,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use xffs_core::{
    BlockDevice, ReadOnlyFs, ReadWriteFs,
    format::{FileKind, FsError, Inode, InodeId},
};
type Result<T> = std::result::Result<T, Errno>;
const MAX_HANDLES: usize = 65536;
fn errno(e: FsError) -> Errno {
    match e {
        FsError::NoSpace | FsError::NoInodes => Errno::ENOSPC,
        FsError::TooBig => Errno::E2BIG,
        FsError::Faulted | FsError::CounterExhausted => Errno::EIO,
        FsError::AlreadyExists => Errno::EEXIST,
        FsError::NotEmpty => Errno::ENOTEMPTY,
        FsError::InvalidInput => Errno::EINVAL,
        FsError::NotFound => Errno::ENOENT,
        FsError::NotDirectory => Errno::ENOTDIR,
        FsError::IsDirectory => Errno::EISDIR,
        FsError::Stale => Errno::ESTALE,
        FsError::InvalidName => Errno::EINVAL,
        FsError::ResourceLimit => Errno::ENOMEM,
        FsError::Unsupported => Errno::EOPNOTSUPP,
        FsError::Device(xffs_core::DeviceError::ReadOnly) => Errno::EROFS,
        FsError::Device(_) | FsError::Corrupt(_) => Errno::EIO,
    }
}
fn kind(k: FileKind) -> FileType {
    match k {
        FileKind::File => FileType::RegularFile,
        FileKind::Directory => FileType::Directory,
    }
}
fn permissions(i: &Inode, noexec: bool) -> u16 {
    if i.kind == FileKind::Directory || (i.executable && !noexec) {
        0o555
    } else {
        0o444
    }
}
fn check_open(flags: OpenFlags) -> Result<()> {
    if flags.0 & (libc::O_ACCMODE | libc::O_TRUNC | libc::O_APPEND | libc::O_CREAT) != 0 {
        Err(Errno::EROFS)
    } else {
        Ok(())
    }
}
enum Volume<D: BlockDevice> {
    ReadOnly(ReadOnlyFs<D>),
    Writable(ReadWriteFs<D>),
}
impl<D: BlockDevice> Volume<D> {
    fn writable(&mut self) -> Result<&mut ReadWriteFs<D>> {
        match self {
            Self::Writable(fs) => Ok(fs),
            _ => Err(Errno::EROFS),
        }
    }
    fn is_writable(&self) -> bool {
        matches!(self, Self::Writable(_))
    }
    fn getattr(&mut self, id: InodeId) -> xffs_core::format::Result<Inode> {
        match self {
            Self::ReadOnly(fs) => fs.getattr(id),
            Self::Writable(fs) => fs.getattr(id),
        }
    }
    fn lookup(&mut self, id: InodeId, name: &[u8]) -> xffs_core::format::Result<InodeId> {
        match self {
            Self::ReadOnly(fs) => fs.lookup(id, name),
            Self::Writable(fs) => fs.lookup(id, name),
        }
    }
    fn read_file(
        &mut self,
        id: InodeId,
        offset: u64,
        size: usize,
    ) -> xffs_core::format::Result<Vec<u8>> {
        match self {
            Self::ReadOnly(fs) => fs.read_file(id, offset, size),
            Self::Writable(fs) => fs.read_file(id, offset, size),
        }
    }
    fn read_dir(
        &mut self,
        id: InodeId,
        cookie: u64,
        limit: usize,
    ) -> xffs_core::format::Result<xffs_core::reader::DirectoryPage> {
        match self {
            Self::ReadOnly(fs) => fs.read_dir(id, cookie, limit),
            Self::Writable(fs) => fs.read_dir(id, cookie, limit),
        }
    }
    fn statfs(&self) -> xffs_core::format::Result<xffs_core::reader::StatFs> {
        match self {
            Self::ReadOnly(fs) => Ok(fs.statfs()),
            Self::Writable(fs) => fs.statfs(),
        }
    }
}
const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
const MAX_NODES: usize = 262144;
struct Handle {
    id: InodeId,
    kind: FileKind,
    flags: i32,
    snapshot: Vec<(INodeNo, FileType, String)>,
    bytes: usize,
}
struct State<D: BlockDevice> {
    fs: Volume<D>,
    handles: BTreeMap<u64, Handle>,
    next_handle: u64,
    nodes: BTreeMap<u64, InodeId>,
    numbers: BTreeMap<InodeId, u64>,
    next_node: u64,
    snapshot_bytes: usize,
}
impl<D: BlockDevice> State<D> {
    fn number(&mut self, id: InodeId) -> Result<INodeNo> {
        if let Some(n) = self.numbers.get(&id) {
            return Ok(INodeNo(*n));
        }
        if self.nodes.len() >= MAX_NODES {
            return Err(Errno::ENOMEM);
        }
        let n = self.next_node;
        self.next_node = n.checked_add(1).ok_or(Errno::ENOMEM)?;
        self.nodes.insert(n, id);
        self.numbers.insert(id, n);
        Ok(INodeNo(n))
    }
    fn id(&self, ino: INodeNo) -> Result<InodeId> {
        self.nodes.get(&ino.0).copied().ok_or(Errno::ENOENT)
    }
    fn handle(&self, ino: INodeNo, fh: FileHandle, expected: FileKind) -> Result<InodeId> {
        let h = self.handles.get(&fh.0).ok_or(Errno::EBADF)?;
        if self.id(ino)? != h.id || h.kind != expected {
            return Err(Errno::EBADF);
        }
        Ok(h.id)
    }
    fn open(&mut self, ino: INodeNo, flags: OpenFlags, expected: FileKind) -> Result<FileHandle> {
        if !self.fs.is_writable() {
            check_open(flags)?;
        }
        if flags.0 & libc::O_ACCMODE == libc::O_ACCMODE {
            return Err(Errno::EINVAL);
        }
        if flags.0 & libc::O_TRUNC != 0 && flags.0 & libc::O_ACCMODE == libc::O_RDONLY {
            return Err(Errno::EACCES);
        }
        let id = self.id(ino)?;
        let i = self.fs.getattr(id).map_err(errno)?;
        if i.kind != expected {
            return Err(if expected == FileKind::Directory {
                Errno::ENOTDIR
            } else {
                Errno::EISDIR
            });
        }
        if self.handles.len() >= MAX_HANDLES {
            return Err(Errno::EMFILE);
        }
        let h = self.next_handle;
        let next_handle = h.checked_add(1).ok_or(Errno::EMFILE)?;
        let mut snapshot = vec![];
        let mut bytes = 0;
        if expected == FileKind::Directory {
            if flags.0 & libc::O_ACCMODE != libc::O_RDONLY {
                return Err(Errno::EISDIR);
            }
            snapshot.push((ino, FileType::Directory, ".".into()));
            snapshot.push((self.number(i.parent)?, FileType::Directory, "..".into()));
            bytes = 256;
            if self.snapshot_bytes + bytes > MAX_SNAPSHOT_BYTES {
                return Err(Errno::ENOMEM);
            }
            let mut cookie = 0;
            loop {
                let page = self.fs.read_dir(id, cookie, 128).map_err(errno)?;
                for r in page.entries {
                    bytes += 128 + r.name.len();
                    if self.snapshot_bytes + bytes > MAX_SNAPSHOT_BYTES {
                        return Err(Errno::ENOMEM);
                    }
                    let k = self.fs.getattr(r.child).map_err(errno)?.kind;
                    snapshot.push((self.number(r.child)?, kind(k), r.name));
                }
                if page.eof {
                    break;
                }
                cookie = page.next_cookie;
            }
        }
        if let Volume::Writable(fs) = &mut self.fs {
            fs.open_handle(id).map_err(errno)?;
            if flags.0 & libc::O_TRUNC != 0
                && let Err(e) = fs.truncate(id, 0)
            {
                let _ = fs.close_handle(id);
                return Err(errno(e));
            }
        }
        self.next_handle = next_handle;
        self.snapshot_bytes += bytes;
        self.handles.insert(
            h,
            Handle {
                id,
                kind: expected,
                flags: flags.0,
                snapshot,
                bytes,
            },
        );
        Ok(FileHandle(h))
    }
    fn sync(&mut self, ino: INodeNo, fh: FileHandle, k: FileKind) -> Result<()> {
        let id = self.handle(ino, fh, k)?;
        self.fs.getattr(id).map_err(errno)?;
        if let Volume::Writable(fs) = &mut self.fs {
            fs.sync().map_err(errno)?;
        }
        Ok(())
    }
    fn release(&mut self, ino: INodeNo, fh: FileHandle, k: FileKind) -> Result<()> {
        let id = self.handle(ino, fh, k)?;
        let h = self.handles.remove(&fh.0).ok_or(Errno::EBADF)?;
        self.snapshot_bytes -= h.bytes;
        if let Volume::Writable(fs) = &mut self.fs {
            fs.close_handle(id).map_err(errno)?;
        }
        Ok(())
    }
    fn read(&mut self, ino: INodeNo, fh: FileHandle, offset: u64, size: u32) -> Result<Vec<u8>> {
        let id = self.handle(ino, fh, FileKind::File)?;
        if self.handles[&fh.0].flags & libc::O_ACCMODE == libc::O_WRONLY {
            return Err(Errno::EBADF);
        }
        self.fs.read_file(id, offset, size as usize).map_err(errno)
    }
    fn write(&mut self, ino: INodeNo, fh: FileHandle, offset: u64, data: &[u8]) -> Result<usize> {
        let id = self.handle(ino, fh, FileKind::File)?;
        let flags = self.handles[&fh.0].flags;
        if flags & libc::O_ACCMODE == libc::O_RDONLY {
            return Err(Errno::EBADF);
        }
        let fs = self.fs.writable()?;
        if flags & libc::O_APPEND != 0 {
            fs.append(id, data).map_err(errno)
        } else {
            fs.write_file(id, offset, data).map_err(errno)
        }
    }
}
pub struct Adapter<D: BlockDevice> {
    state: Mutex<State<D>>,
    uid: u32,
    gid: u32,
    noexec: bool,
    writable: bool,
    revision_two: bool,
}
impl<D: BlockDevice> Adapter<D> {
    pub fn new(fs: ReadOnlyFs<D>, uid: u32, gid: u32, noexec: bool) -> Self {
        Self::from_volume(Volume::ReadOnly(fs), uid, gid, noexec)
    }
    pub fn new_writable(fs: ReadWriteFs<D>, uid: u32, gid: u32, noexec: bool) -> Self {
        Self::from_volume(Volume::Writable(fs), uid, gid, noexec)
    }
    fn from_volume(fs: Volume<D>, uid: u32, gid: u32, noexec: bool) -> Self {
        let writable = fs.is_writable();
        let revision_two = match &fs {
            Volume::ReadOnly(fs) => {
                fs.superblock().revision == xffs_core::format::FormatRevision::Two
            }
            Volume::Writable(_) => true,
        };
        Self {
            state: Mutex::new(State {
                fs,
                handles: BTreeMap::new(),
                next_handle: 1,
                nodes: BTreeMap::from([(1, xffs_core::format::ROOT)]),
                numbers: BTreeMap::from([(xffs_core::format::ROOT, 1)]),
                next_node: 2,
                snapshot_bytes: 0,
            }),
            uid,
            gid,
            noexec,
            writable,
            revision_two,
        }
    }
    fn with<T>(&self, f: impl FnOnce(&mut State<D>) -> Result<T>) -> Result<T> {
        let mut state = self.state.lock().map_err(|_| Errno::EIO)?;
        f(&mut state)
    }
    fn attr(&self, i: &Inode, ino: INodeNo) -> Result<FileAttr> {
        let time = |t| {
            UNIX_EPOCH
                .checked_add(Duration::from_secs(t))
                .ok_or(Errno::EIO)
        };
        Ok(FileAttr {
            ino,
            size: i.size,
            blocks: i.allocated.checked_mul(8).ok_or(Errno::EIO)?,
            atime: time(if self.revision_two {
                i.atime
            } else {
                i.times[1]
            })?,
            mtime: time(i.times[1])?,
            ctime: time(i.times[2])?,
            crtime: time(i.times[0])?,
            kind: kind(i.kind),
            perm: if self.writable {
                if i.kind == FileKind::Directory || i.executable {
                    0o755
                } else {
                    0o644
                }
            } else {
                permissions(i, self.noexec)
            },
            nlink: if i.kind == FileKind::Directory { 2 } else { 1 },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        })
    }
    fn empty(reply: ReplyEmpty, result: Result<()>) {
        match result {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }
}
pub fn mount_config(noexec: bool) -> Config {
    mount_config_writable(noexec, false)
}
pub fn mount_config_writable(noexec: bool, writable: bool) -> Config {
    let mut config = Config::default();
    config.n_threads = Some(1);
    config.mount_options = vec![
        if writable {
            MountOption::RW
        } else {
            MountOption::RO
        },
        MountOption::NoSuid,
        MountOption::NoDev,
        MountOption::DefaultPermissions,
        MountOption::NoAtime,
        MountOption::FSName("xffs".into()),
        MountOption::CUSTOM("max_read=1048576".into()),
    ];
    if noexec {
        config.mount_options.push(MountOption::NoExec);
    }
    config
}
impl<D: BlockDevice + Send + 'static> Filesystem for Adapter<D> {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> std::io::Result<()> {
        // fuser's default requested capabilities omit WRITEBACK_CACHE. Never add it.
        config
            .set_max_write(xffs_core::reader::MAX_READ as u32)
            .map_err(|_| std::io::Error::other("FUSE maximum request size"))?;
        Ok(())
    }
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let result = self.with(|s| {
            let parent = s.id(parent)?;
            let p = s.fs.getattr(parent).map_err(errno)?;
            if p.kind != FileKind::Directory {
                return Err(Errno::ENOTDIR);
            }
            let id = match name.as_bytes() {
                b"." => parent,
                b".." => p.parent,
                n => s.fs.lookup(parent, n).map_err(errno)?,
            };
            let i = s.fs.getattr(id).map_err(errno)?;
            Ok((self.attr(&i, s.number(i.id)?)?, Generation(id.generation)))
        });
        match result {
            Ok((attr, generation)) => reply.entry(&Duration::ZERO, &attr, generation),
            Err(e) => reply.error(e),
        }
    }
    fn getattr(&self, _req: &Request, ino: INodeNo, fh: Option<FileHandle>, reply: ReplyAttr) {
        let result = self.with(|s| {
            let id = s.id(ino)?;
            let i = s.fs.getattr(id).map_err(errno)?;
            if let Some(fh) = fh {
                s.handle(ino, fh, i.kind)?;
            }
            self.attr(&i, ino)
        });
        match result {
            Ok(a) => reply.attr(&Duration::ZERO, &a),
            Err(e) => reply.error(e),
        }
    }
    fn access(&self, _req: &Request, ino: INodeNo, mask: AccessFlags, reply: ReplyEmpty) {
        Self::empty(
            reply,
            self.with(|s| {
                let id = s.id(ino)?;
                let i = s.fs.getattr(id).map_err(errno)?;
                if mask.contains(AccessFlags::W_OK) && !self.writable {
                    return Err(Errno::EROFS);
                }
                if mask.contains(AccessFlags::X_OK) && permissions(&i, self.noexec) & 0o111 == 0 {
                    return Err(Errno::EACCES);
                }
                Ok(())
            }),
        );
    }
    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        match self.with(|s| s.open(ino, flags, FileKind::File)) {
            Ok(h) => reply.opened(
                h,
                if self.writable {
                    FopenFlags::FOPEN_DIRECT_IO
                } else {
                    FopenFlags::empty()
                },
            ),
            Err(e) => reply.error(e),
        }
    }
    fn opendir(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        match self.with(|s| s.open(ino, flags, FileKind::Directory)) {
            Ok(h) => reply.opened(h, FopenFlags::empty()),
            Err(e) => reply.error(e),
        }
    }
    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        match self.with(|s| s.read(ino, fh, offset, size)) {
            Ok(b) => reply.data(&b),
            Err(e) => reply.error(e),
        }
    }
    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let result = self.with(|s| {
            s.handle(ino, fh, FileKind::Directory)?;
            s.fs.statfs().map_err(errno)?;
            let snapshot = &s.handles[&fh.0].snapshot;
            let start = usize::try_from(offset).map_err(|_| Errno::EINVAL)?;
            if start > snapshot.len() {
                return Err(Errno::EINVAL);
            }
            for (index, (number, k, name)) in snapshot.iter().enumerate().skip(start) {
                if reply.add(*number, (index + 1) as u64, *k, name) {
                    break;
                }
            }
            Ok(())
        });
        match result {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }
    fn flush(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        Self::empty(reply, self.with(|s| s.sync(ino, fh, FileKind::File)));
    }
    fn fsync(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        Self::empty(reply, self.with(|s| s.sync(ino, fh, FileKind::File)));
    }
    fn fsyncdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        Self::empty(reply, self.with(|s| s.sync(ino, fh, FileKind::Directory)));
    }
    fn release(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        Self::empty(reply, self.with(|s| s.release(ino, fh, FileKind::File)));
    }
    fn releasedir(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        reply: ReplyEmpty,
    ) {
        Self::empty(
            reply,
            self.with(|s| s.release(ino, fh, FileKind::Directory)),
        );
    }
    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        match self.with(|s| s.fs.statfs().map_err(errno)) {
            Ok(s) => reply.statfs(
                s.blocks,
                s.free_blocks,
                s.free_blocks,
                s.inodes,
                s.free_inodes,
                s.block_size,
                s.max_name,
                s.block_size,
            ),
            Err(e) => reply.error(e),
        }
    }
    fn readlink(&self, _req: &Request, _ino: INodeNo, reply: ReplyData) {
        reply.error(Errno::EINVAL);
    }
    fn getxattr(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _name: &OsStr,
        _size: u32,
        reply: ReplyXattr,
    ) {
        reply.error(Errno::EOPNOTSUPP);
    }
    fn listxattr(&self, _req: &Request, _ino: INodeNo, _size: u32, reply: ReplyXattr) {
        reply.error(Errno::EOPNOTSUPP);
    }
    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        ctime: Option<SystemTime>,
        fh: Option<FileHandle>,
        crtime: Option<SystemTime>,
        chgtime: Option<SystemTime>,
        bkuptime: Option<SystemTime>,
        flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let result = self.with(|s| {
            s.fs.writable()?;
            let id = s.id(ino)?;
            let i = s.fs.getattr(id).map_err(errno)?;
            if uid.is_some_and(|v| v != self.uid) || gid.is_some_and(|v| v != self.gid) {
                return Err(Errno::EPERM);
            }
            if crtime.is_some() || chgtime.is_some() || bkuptime.is_some() || flags.is_some() {
                return Err(Errno::EOPNOTSUPP);
            }
            // Linux supplies ctime as a consequence of setattr, not a user-controlled field.
            let _ = ctime;
            let executable = mode
                .map(|m| chmod_flag(&i, m))
                .transpose()?
                .filter(|_| i.kind == FileKind::File);
            let atime = atime.map(timestamp).transpose()?;
            let mtime = mtime.map(timestamp).transpose()?;
            if let Some(h) = fh {
                s.handle(ino, h, i.kind)?;
                if size.is_some() && s.handles[&h.0].flags & libc::O_ACCMODE == libc::O_RDONLY {
                    return Err(Errno::EBADF);
                }
            }
            let fs = s.fs.writable()?;
            if let Some(size) = size {
                fs.truncate(id, size).map_err(errno)?;
            }
            if executable.is_some() || atime.is_some() || mtime.is_some() {
                fs.set_attributes(id, executable, atime, mtime)
                    .map_err(errno)?;
            }
            self.attr(&fs.getattr(id).map_err(errno)?, ino)
        });
        match result {
            Ok(a) => reply.attr(&Duration::ZERO, &a),
            Err(e) => reply.error(e),
        }
    }

    fn mknod(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        let result = self.with(|s| {
            let parent = s.id(parent)?;
            let fs = s.fs.writable()?;
            if mode & libc::S_IFMT != libc::S_IFREG {
                return Err(Errno::EOPNOTSUPP);
            }
            let id = fs
                .create(parent, name.as_bytes(), mode & 0o111 != 0)
                .map_err(errno)?;
            let i = fs.getattr(id).map_err(errno)?;
            Ok((self.attr(&i, s.number(id)?)?, Generation(id.generation)))
        });
        match result {
            Ok((a, g)) => reply.entry(&Duration::ZERO, &a, g),
            Err(e) => reply.error(e),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let result = self.with(|s| {
            let parent = s.id(parent)?;
            let fs = s.fs.writable()?;
            let id = fs.mkdir(parent, name.as_bytes()).map_err(errno)?;
            let i = fs.getattr(id).map_err(errno)?;
            Ok((self.attr(&i, s.number(id)?)?, Generation(id.generation)))
        });
        match result {
            Ok((a, g)) => reply.entry(&Duration::ZERO, &a, g),
            Err(e) => reply.error(e),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        Self::empty(
            reply,
            self.with(|s| {
                let parent = s.id(parent)?;
                s.fs.writable()?
                    .unlink(parent, name.as_bytes())
                    .map_err(errno)
            }),
        );
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        Self::empty(
            reply,
            self.with(|s| {
                let parent = s.id(parent)?;
                s.fs.writable()?
                    .rmdir(parent, name.as_bytes())
                    .map_err(errno)
            }),
        );
    }

    fn symlink(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _link_name: &OsStr,
        _target: &Path,
        reply: ReplyEntry,
    ) {
        reply.error(if self.writable {
            Errno::EOPNOTSUPP
        } else {
            Errno::EROFS
        });
    }
    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        Self::empty(
            reply,
            self.with(|s| {
                let parent = s.id(parent)?;
                let newparent = s.id(newparent)?;
                let fs = s.fs.writable()?;
                if flags.bits() & !RenameFlags::RENAME_NOREPLACE.bits() != 0 {
                    return Err(Errno::EOPNOTSUPP);
                }
                fs.rename(
                    parent,
                    name.as_bytes(),
                    newparent,
                    newname.as_bytes(),
                    flags.contains(RenameFlags::RENAME_NOREPLACE),
                )
                .map_err(errno)
            }),
        );
    }

    fn link(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _newparent: INodeNo,
        _newname: &OsStr,
        reply: ReplyEntry,
    ) {
        reply.error(if self.writable {
            Errno::EOPNOTSUPP
        } else {
            Errno::EROFS
        });
    }
    fn write(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        match self.with(|s| s.write(ino, fh, offset, data)) {
            Ok(n) => reply.written(n as u32),
            Err(e) => reply.error(e),
        }
    }

    fn setxattr(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _name: &OsStr,
        _value: &[u8],
        _flags: i32,
        _position: u32,
        reply: ReplyEmpty,
    ) {
        reply.error(if self.writable {
            Errno::EOPNOTSUPP
        } else {
            Errno::EROFS
        });
    }
    fn removexattr(&self, _req: &Request, _ino: INodeNo, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(if self.writable {
            Errno::EOPNOTSUPP
        } else {
            Errno::EROFS
        });
    }
    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let result = self.with(|s| {
            s.fs.writable()?;
            if s.handles.len() >= MAX_HANDLES {
                return Err(Errno::EMFILE);
            }
            if s.nodes.len() >= MAX_NODES {
                return Err(Errno::ENOMEM);
            }
            if flags & libc::O_ACCMODE == libc::O_ACCMODE {
                return Err(Errno::EINVAL);
            }
            let parent = s.id(parent)?;
            let id =
                s.fs.writable()?
                    .create(parent, name.as_bytes(), mode & 0o111 != 0)
                    .map_err(errno)?;
            let ino = s.number(id)?;
            let h = s.open(ino, OpenFlags(flags & !libc::O_TRUNC), FileKind::File)?;
            let i = s.fs.getattr(id).map_err(errno)?;
            Ok((self.attr(&i, ino)?, Generation(id.generation), h))
        });
        match result {
            Ok((a, g, h)) => reply.created(&Duration::ZERO, &a, g, h, FopenFlags::FOPEN_DIRECT_IO),
            Err(e) => reply.error(e),
        }
    }

    fn fallocate(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _offset: u64,
        _length: u64,
        _mode: i32,
        reply: ReplyEmpty,
    ) {
        reply.error(if self.writable {
            Errno::EOPNOTSUPP
        } else {
            Errno::EROFS
        });
    }
    fn copy_file_range(
        &self,
        _req: &Request,
        _ino_in: INodeNo,
        _fh_in: FileHandle,
        _offset_in: u64,
        _ino_out: INodeNo,
        _fh_out: FileHandle,
        _offset_out: u64,
        _len: u64,
        _flags: CopyFileRangeFlags,
        reply: ReplyWrite,
    ) {
        reply.error(if self.writable {
            Errno::EOPNOTSUPP
        } else {
            Errno::EROFS
        });
    }
}

fn timestamp(t: TimeOrNow) -> Result<u64> {
    let t = match t {
        TimeOrNow::Now => SystemTime::now(),
        TimeOrNow::SpecificTime(t) => t,
    };
    let seconds = t
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Errno::EINVAL)?
        .as_secs();
    if seconds > i64::MAX as u64 {
        return Err(Errno::EINVAL);
    }
    Ok(seconds)
}
fn chmod_flag(i: &Inode, mode: u32) -> Result<bool> {
    let mode = mode & !libc::S_IFMT;
    if i.kind == FileKind::Directory {
        if mode != 0o755 {
            return Err(Errno::EOPNOTSUPP);
        }
        // Directory execution is fixed, so no core flag update is needed.
        return Ok(false);
    }
    if mode & !0o111 != 0o644 {
        return Err(Errno::EOPNOTSUPP);
    }
    Ok(mode & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use xffs_core::{DeviceError, reader::OpenOptions};
    struct Spy(Arc<Vec<u8>>);
    impl BlockDevice for Spy {
        fn capacity_bytes(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_at(&mut self, o: u64, b: &mut [u8]) -> std::result::Result<(), DeviceError> {
            xffs_core::validate_range(self.capacity_bytes(), o, b.len())?;
            b.copy_from_slice(&self.0[o as usize..o as usize + b.len()]);
            Ok(())
        }
        fn write_at(&mut self, _: u64, _: &[u8]) -> std::result::Result<(), DeviceError> {
            panic!("adapter wrote backend")
        }
        fn flush(&mut self) -> std::result::Result<(), DeviceError> {
            panic!("adapter flushed backend")
        }
    }
    #[test]
    fn handles_permissions_sync_and_resource_limits_without_fuse() {
        let p = std::env::temp_dir().join(format!("xffs-adapter-{}.img", std::process::id()));
        xffs_tools::create_image(
            &p,
            16 * 1024 * 1024,
            xffs_tools::DEMO_UUID,
            None,
            Some(xffs_tools::Scenario::Committed),
        )
        .unwrap();
        let bytes = Arc::new(std::fs::read(&p).unwrap());
        std::fs::remove_file(p).unwrap();
        let fs = ReadOnlyFs::from_device(Spy(bytes), OpenOptions::default()).unwrap();
        let adapter = Adapter::new(fs, 123, 456, false);
        adapter
            .with(|s| {
                let id =
                    s.fs.lookup(xffs_core::format::ROOT, b"ReadMe.txt")
                        .map_err(errno)?;
                let ino = s.number(id)?;
                let h = s.open(ino, OpenFlags(libc::O_RDONLY), FileKind::File)?;
                assert_eq!(s.read(ino, h, 0, 4096)?, b"Hello from XFFS!\n");
                s.sync(ino, h, FileKind::File)?;
                s.sync(ino, h, FileKind::File)?;
                assert!(s.handle(INodeNo::ROOT, h, FileKind::File).is_err());
                assert!(s.handle(ino, h, FileKind::Directory).is_err());
                s.release(ino, h, FileKind::File)?;
                assert!(s.read(ino, h, 0, 1).is_err());
                for flags in [
                    libc::O_WRONLY,
                    libc::O_RDWR,
                    libc::O_TRUNC,
                    libc::O_APPEND,
                    libc::O_CREAT,
                ] {
                    assert_eq!(
                        s.open(ino, OpenFlags(flags), FileKind::File)
                            .unwrap_err()
                            .code(),
                        libc::EROFS
                    );
                }
                let root = s.open(INodeNo::ROOT, OpenFlags(0), FileKind::Directory)?;
                s.sync(INodeNo::ROOT, root, FileKind::Directory)?;
                s.release(INodeNo::ROOT, root, FileKind::Directory)?;
                let exec =
                    s.fs.lookup(xffs_core::format::ROOT, b"run.sh")
                        .map_err(errno)?;
                let i = s.fs.getattr(exec).map_err(errno)?;
                let attr = adapter.attr(&i, s.number(i.id)?)?;
                assert_eq!((attr.uid, attr.gid, attr.perm), (123, 456, 0o555));
                assert_eq!(permissions(&i, true), 0o444);
                for _ in 0..MAX_HANDLES {
                    s.open(ino, OpenFlags(0), FileKind::File)?;
                }
                assert_eq!(
                    s.open(ino, OpenFlags(0), FileKind::File)
                        .unwrap_err()
                        .code(),
                    libc::EMFILE
                );
                Ok(())
            })
            .unwrap();
        let c = mount_config(true);
        assert_eq!(c.n_threads, Some(1));
        for required in [
            MountOption::RO,
            MountOption::NoSuid,
            MountOption::NoDev,
            MountOption::NoExec,
            MountOption::DefaultPermissions,
        ] {
            assert!(c.mount_options.contains(&required));
        }
    }
    #[test]
    fn writable_handles_generations_append_snapshots_and_modes() {
        let p = std::env::temp_dir().join(format!("xffs-rw-adapter-{}.img", std::process::id()));
        xffs_tools::create_image(&p, 16 * 1024 * 1024, xffs_tools::DEMO_UUID, None, None).unwrap();
        let fs = ReadWriteFs::open(&p).unwrap();
        let adapter = Adapter::new_writable(fs, 123, 456, false);
        adapter
            .with(|s| {
                let id =
                    s.fs.writable()?
                        .create(xffs_core::format::ROOT, b"file", false)
                        .map_err(errno)?;
                let ino = s.number(id)?;
                let r = s.open(ino, OpenFlags(libc::O_RDONLY), FileKind::File)?;
                let w = s.open(
                    ino,
                    OpenFlags(libc::O_WRONLY | libc::O_APPEND),
                    FileKind::File,
                )?;
                assert_eq!(s.write(ino, r, 0, b"bad").unwrap_err().code(), libc::EBADF);
                assert_eq!(s.read(ino, w, 0, 1).unwrap_err().code(), libc::EBADF);
                s.write(ino, w, 999, b"a")?;
                s.write(ino, w, 0, b"b")?;
                assert_eq!(s.read(ino, r, 0, 8)?, b"ab");
                let dir = s.open(INodeNo::ROOT, OpenFlags(0), FileKind::Directory)?;
                let snapshot = s.handles[&dir.0].snapshot.clone();
                s.fs.writable()?
                    .unlink(xffs_core::format::ROOT, b"file")
                    .map_err(errno)?;
                assert_eq!(s.handles[&dir.0].snapshot, snapshot);
                s.write(ino, w, 0, b"c")?;
                assert_eq!(s.read(ino, r, 0, 8)?, b"abc");
                s.sync(ino, w, FileKind::File)?;
                s.release(ino, w, FileKind::File)?;
                s.release(ino, r, FileKind::File)?;
                let new =
                    s.fs.writable()?
                        .create(xffs_core::format::ROOT, b"new", false)
                        .map_err(errno)?;
                assert_eq!(id.index, new.index);
                assert_ne!(id.generation, new.generation);
                assert_ne!(s.number(new)?, ino);
                assert!(s.fs.getattr(s.id(ino)?).is_err());
                s.release(INodeNo::ROOT, dir, FileKind::Directory)?;
                assert_eq!(s.snapshot_bytes, 0);
                s.snapshot_bytes = MAX_SNAPSHOT_BYTES;
                assert_eq!(
                    s.open(INodeNo::ROOT, OpenFlags(0), FileKind::Directory)
                        .unwrap_err()
                        .code(),
                    libc::ENOMEM
                );
                s.snapshot_bytes = 0;

                let i = s.fs.getattr(new).map_err(errno)?;
                let attr = adapter.attr(&i, s.number(new)?)?;
                assert_eq!((attr.uid, attr.gid, attr.perm), (123, 456, 0o644));
                assert!(chmod_flag(&i, 0o744).unwrap());
                assert!(!chmod_flag(&i, 0o644).unwrap());
                assert_eq!(chmod_flag(&i, 0o600).unwrap_err().code(), libc::EOPNOTSUPP);
                assert_eq!(chmod_flag(&i, 0o4755).unwrap_err().code(), libc::EOPNOTSUPP);
                let n = s.number(new)?;
                let h = s.open(n, OpenFlags(libc::O_RDWR), FileKind::File)?;
                s.write(n, h, 0, b"before")?;
                s.release(n, h, FileKind::File)?;
                let h = s.open(n, OpenFlags(libc::O_WRONLY | libc::O_TRUNC), FileKind::File)?;
                assert_eq!(s.fs.getattr(new).map_err(errno)?.size, 0);
                s.release(n, h, FileKind::File)?;
                assert_eq!(
                    s.open(n, OpenFlags(libc::O_RDONLY | libc::O_TRUNC), FileKind::File)
                        .unwrap_err()
                        .code(),
                    libc::EACCES
                );
                Ok(())
            })
            .unwrap();
        drop(adapter);
        xffs_core::ReadOnlyFs::open(&p).unwrap();
        std::fs::remove_file(p).unwrap();
        assert!(
            mount_config_writable(true, true)
                .mount_options
                .contains(&MountOption::RW)
        );
        assert!(timestamp(TimeOrNow::SpecificTime(UNIX_EPOCH - Duration::from_secs(1))).is_err());
    }
}
