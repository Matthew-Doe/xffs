//! Linux read-only FUSE adapter. All operations serialize through one state lock.
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
    BlockDevice, ReadOnlyFs,
    format::{FileKind, FsError, Inode, InodeId},
};
type Result<T> = std::result::Result<T, Errno>;
const MAX_HANDLES: usize = 65536;
fn errno(e: FsError) -> Errno {
    match e {
        FsError::NotFound => Errno::ENOENT,
        FsError::NotDirectory => Errno::ENOTDIR,
        FsError::IsDirectory => Errno::EISDIR,
        FsError::Stale => Errno::ESTALE,
        FsError::InvalidName => Errno::EINVAL,
        FsError::ResourceLimit => Errno::ENOMEM,
        FsError::Unsupported => Errno::EOPNOTSUPP,
        FsError::Device(_) | FsError::Corrupt(_) => Errno::EIO,
    }
}
fn fuse_ino(id: InodeId) -> INodeNo {
    INodeNo(id.index + 1)
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
struct State<D: BlockDevice> {
    fs: ReadOnlyFs<D>,
    handles: BTreeMap<u64, (InodeId, FileKind)>,
    next_handle: u64,
}
impl<D: BlockDevice> State<D> {
    fn id(&self, ino: INodeNo) -> Result<InodeId> {
        let index = ino.0.checked_sub(1).ok_or(Errno::ENOENT)?;
        self.fs.inode_id(index).map_err(errno)
    }
    fn handle(&self, ino: INodeNo, fh: FileHandle, expected: FileKind) -> Result<InodeId> {
        let &(id, k) = self.handles.get(&fh.0).ok_or(Errno::EBADF)?;
        if fuse_ino(id) != ino || k != expected || self.id(ino)? != id {
            return Err(Errno::EBADF);
        }
        Ok(id)
    }
    fn open(&mut self, ino: INodeNo, flags: OpenFlags, expected: FileKind) -> Result<FileHandle> {
        check_open(flags)?;
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
        self.next_handle = h.checked_add(1).ok_or(Errno::EMFILE)?;
        self.handles.insert(h, (id, expected));
        Ok(FileHandle(h))
    }
    fn sync(&mut self, ino: INodeNo, fh: FileHandle, k: FileKind) -> Result<()> {
        let id = self.handle(ino, fh, k)?;
        self.fs.getattr(id).map_err(errno)?;
        Ok(())
    }
    fn release(&mut self, ino: INodeNo, fh: FileHandle, k: FileKind) -> Result<()> {
        self.handle(ino, fh, k)?;
        self.handles.remove(&fh.0);
        Ok(())
    }
    fn read(&mut self, ino: INodeNo, fh: FileHandle, offset: u64, size: u32) -> Result<Vec<u8>> {
        let id = self.handle(ino, fh, FileKind::File)?;
        self.fs.read_file(id, offset, size as usize).map_err(errno)
    }
}
pub struct Adapter<D: BlockDevice> {
    state: Mutex<State<D>>,
    uid: u32,
    gid: u32,
    noexec: bool,
}
impl<D: BlockDevice> Adapter<D> {
    pub fn new(fs: ReadOnlyFs<D>, uid: u32, gid: u32, noexec: bool) -> Self {
        Self {
            state: Mutex::new(State {
                fs,
                handles: BTreeMap::new(),
                next_handle: 1,
            }),
            uid,
            gid,
            noexec,
        }
    }
    fn with<T>(&self, f: impl FnOnce(&mut State<D>) -> Result<T>) -> Result<T> {
        let mut state = self.state.lock().map_err(|_| Errno::EIO)?;
        f(&mut state)
    }
    fn attr(&self, i: &Inode) -> Result<FileAttr> {
        let time = |t| {
            UNIX_EPOCH
                .checked_add(Duration::from_secs(t))
                .ok_or(Errno::EIO)
        };
        Ok(FileAttr {
            ino: fuse_ino(i.id),
            size: i.size,
            blocks: i.allocated.checked_mul(8).ok_or(Errno::EIO)?,
            atime: time(i.times[1])?,
            mtime: time(i.times[1])?,
            ctime: time(i.times[2])?,
            crtime: time(i.times[0])?,
            kind: kind(i.kind),
            perm: permissions(i, self.noexec),
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
    let mut config = Config::default();
    config.n_threads = Some(1);
    config.mount_options = vec![
        MountOption::RO,
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
            Ok((self.attr(&i)?, Generation(id.generation)))
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
            self.attr(&i)
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
                if mask.contains(AccessFlags::W_OK) {
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
            Ok(h) => reply.opened(h, FopenFlags::empty()),
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
            let id = s.handle(ino, fh, FileKind::Directory)?;
            let i = s.fs.getattr(id).map_err(errno)?;
            if offset == 0 && reply.add(ino, 1, FileType::Directory, ".") {
                return Ok(());
            }
            if offset <= 1 && reply.add(fuse_ino(i.parent), 2, FileType::Directory, "..") {
                return Ok(());
            }
            let mut cookie = offset.saturating_sub(2);
            loop {
                let page = s.fs.read_dir(id, cookie, 128).map_err(errno)?;
                for (n, r) in page.entries.iter().enumerate() {
                    let attr = s.fs.getattr(r.child).map_err(errno)?;
                    if reply.add(
                        fuse_ino(r.child),
                        cookie + n as u64 + 3,
                        kind(attr.kind),
                        &r.name,
                    ) {
                        return Ok(());
                    }
                }
                cookie = page.next_cookie;
                if page.eof {
                    return Ok(());
                }
            }
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
        match self.with(|s| Ok(s.fs.statfs())) {
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
        _ino: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        _size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        reply.error(Errno::EROFS);
    }
    fn mknod(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        reply.error(Errno::EROFS);
    }
    fn mkdir(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        reply.error(Errno::EROFS);
    }
    fn unlink(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(Errno::EROFS);
    }
    fn rmdir(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(Errno::EROFS);
    }
    fn symlink(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _link_name: &OsStr,
        _target: &Path,
        reply: ReplyEntry,
    ) {
        reply.error(Errno::EROFS);
    }
    fn rename(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _newparent: INodeNo,
        _newname: &OsStr,
        _flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        reply.error(Errno::EROFS);
    }
    fn link(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _newparent: INodeNo,
        _newname: &OsStr,
        reply: ReplyEntry,
    ) {
        reply.error(Errno::EROFS);
    }
    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _offset: u64,
        _data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        reply.error(Errno::EROFS);
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
        reply.error(Errno::EROFS);
    }
    fn removexattr(&self, _req: &Request, _ino: INodeNo, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(Errno::EROFS);
    }
    fn create(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        reply.error(Errno::EROFS);
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
        reply.error(Errno::EROFS);
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
        reply.error(Errno::EROFS);
    }
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
                let ino = fuse_ino(id);
                assert_eq!(ino.0, id.index + 1);
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
                let attr = adapter.attr(&i)?;
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
}
