use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use crate::apis::{ApiClient, FileEntry};
use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyWrite, Request, Session, SessionUnmounter,
};
use libc::ENOENT;

use crate::cache::{CacheManager, CHUNK_SIZE, METADATA_CACHE_TTL};

const FUSE_TTL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
struct INode {
    ino: u64,
    path: String,
    attr: FileAttr,
    cached_at: Instant,
}

impl INode {
    fn is_metadata_expired(&self) -> bool {
        self.cached_at.elapsed() > METADATA_CACHE_TTL
    }
}

struct InodeTable {
    inodes: HashMap<u64, INode>,
    path_to_ino: HashMap<String, u64>,
    next_ino: u64,
    uid: u32,
    gid: u32,
}

impl InodeTable {
    fn new(uid: u32, gid: u32) -> Self {
        let mut table = Self {
            inodes: HashMap::new(),
            path_to_ino: HashMap::new(),
            next_ino: 2,
            uid,
            gid,
        };

        let root_attr = FileAttr {
            ino: 1,
            size: 0,
            blocks: 0,
            atime: SystemTime::now(),
            mtime: SystemTime::now(),
            ctime: SystemTime::now(),
            crtime: SystemTime::now(),
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid,
            gid,
            rdev: 0,
            flags: 0,
            blksize: 512,
        };

        table.inodes.insert(
            1,
            INode {
                ino: 1,
                path: "/".to_string(),
                attr: root_attr,
                cached_at: Instant::now(),
            },
        );
        table.path_to_ino.insert("/".to_string(), 1);

        table
    }

    fn get_or_create(&mut self, path: &str, entry: &FileEntry) -> u64 {
        if let Some(&ino) = self.path_to_ino.get(path) {
            if let Some(inode) = self.inodes.get_mut(&ino) {
                inode.attr.size = entry.size;
                inode.attr.mtime = UNIX_EPOCH + Duration::from_secs_f64(entry.mtime);
                inode.attr.ctime = UNIX_EPOCH + Duration::from_secs_f64(entry.ctime);
                inode.cached_at = Instant::now();
            }
            return ino;
        }

        let ino = self.next_ino;
        self.next_ino += 1;

        let server_perm = (entry.mode & 0o777) as u16;
        let perm = if server_perm == 0 {
            if entry.is_dir {
                0o755
            } else {
                0o644
            }
        } else if entry.is_dir {
            server_perm | 0o700
        } else {
            server_perm | 0o600
        };

        let attr = FileAttr {
            ino,
            size: entry.size,
            blocks: entry.size.div_ceil(512),
            atime: UNIX_EPOCH + Duration::from_secs_f64(entry.mtime),
            mtime: UNIX_EPOCH + Duration::from_secs_f64(entry.mtime),
            ctime: UNIX_EPOCH + Duration::from_secs_f64(entry.ctime),
            crtime: UNIX_EPOCH + Duration::from_secs_f64(entry.ctime),
            kind: if entry.is_dir {
                FileType::Directory
            } else {
                FileType::RegularFile
            },
            perm,
            nlink: if entry.is_dir { 2 } else { 1 },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            flags: 0,
            blksize: 512,
        };

        self.inodes.insert(
            ino,
            INode {
                ino,
                path: path.to_string(),
                attr,
                cached_at: Instant::now(),
            },
        );
        self.path_to_ino.insert(path.to_string(), ino);
        ino
    }

    fn get(&self, ino: u64) -> Option<&INode> {
        self.inodes.get(&ino)
    }

    fn get_cloned(&self, ino: u64) -> Option<INode> {
        self.inodes.get(&ino).cloned()
    }

    fn get_mut(&mut self, ino: u64) -> Option<&mut INode> {
        self.inodes.get_mut(&ino)
    }

    fn child_path(&self, parent: u64, name: &OsStr) -> Option<String> {
        let parent_inode = self.inodes.get(&parent)?;
        let name_str = name.to_str()?;

        if parent_inode.path == "/" {
            Some(format!("/{name_str}"))
        } else {
            Some(format!("{}/{}", parent_inode.path, name_str))
        }
    }

    fn remove_by_path(&mut self, path: &str) {
        if let Some(ino) = self.path_to_ino.remove(path) {
            self.inodes.remove(&ino);
        }
    }

    fn rename(&mut self, from: &str, to: &str) {
        if let Some(ino) = self.path_to_ino.remove(from) {
            self.path_to_ino.insert(to.to_string(), ino);
            if let Some(inode) = self.inodes.get_mut(&ino) {
                inode.path = to.to_string();
                inode.cached_at = Instant::now();
            }
        }
    }

    fn invalidate_metadata(&mut self, path: &str) {
        if let Some(&ino) = self.path_to_ino.get(path)
            && let Some(inode) = self.inodes.get_mut(&ino)
        {
            inode.cached_at = Instant::now() - METADATA_CACHE_TTL - Duration::from_secs(1);
        }
    }
}

struct FileHandleTable {
    handles: HashMap<u64, String>,
    next_fh: u64,
}

impl FileHandleTable {
    fn new() -> Self {
        Self {
            handles: HashMap::new(),
            next_fh: 1,
        }
    }

    fn open(&mut self, path: String) -> u64 {
        let fh = self.next_fh;
        self.next_fh += 1;
        self.handles.insert(fh, path);
        fh
    }

    fn close(&mut self, fh: u64) {
        self.handles.remove(&fh);
    }
}

struct PathLockManager {
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl PathLockManager {
    fn new() -> Self {
        Self {
            locks: Mutex::new(HashMap::new()),
        }
    }

    fn get_lock(&self, path: &str) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().unwrap();
        locks
            .entry(path.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

pub struct RemoteFS {
    api_client: Arc<ApiClient>,
    inode_table: Arc<RwLock<InodeTable>>,
    file_handles: Arc<Mutex<FileHandleTable>>,
    cache: Arc<Mutex<CacheManager>>,
    path_locks: Arc<PathLockManager>,
}

impl RemoteFS {
    pub fn new(api_client: ApiClient) -> Self {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };

        Self {
            api_client: Arc::new(api_client),
            inode_table: Arc::new(RwLock::new(InodeTable::new(uid, gid))),
            file_handles: Arc::new(Mutex::new(FileHandleTable::new())),
            cache: Arc::new(Mutex::new(CacheManager::new())),
            path_locks: Arc::new(PathLockManager::new()),
        }
    }

    fn invalidate_all_for_path(&self, path: &str) {
        self.cache.lock().unwrap().invalidate_all_for_path(path);
        self.inode_table.write().unwrap().invalidate_metadata(path);
    }

    pub fn mount(
        self,
        mountpoint: &Path,
        unmounter_slot: Arc<Mutex<Option<SessionUnmounter>>>,
    ) -> Result<()> {
        let mut options = vec![
            MountOption::FSName("remoteFS".to_string()),
            MountOption::AutoUnmount,
        ];

        #[cfg(target_os = "linux")]
        {
            options.push(MountOption::AllowOther);
            options.push(MountOption::DefaultPermissions);
        }

        #[cfg(target_os = "macos")]
        {
            options.push(MountOption::RW);
        }

        log::info!("Mounting filesystem at {}", mountpoint.display());

        let mut session = Session::new(self, mountpoint, &options)
            .map_err(|e| anyhow::anyhow!("Failed to create FUSE session: {e}"))?;

        {
            let mut guard = unmounter_slot.lock().unwrap();
            *guard = Some(session.unmount_callable());
        }

        session
            .run()
            .map_err(|e| anyhow::anyhow!("FUSE session error: {e}"))?;

        Ok(())
    }
}

impl Filesystem for RemoteFS {
    fn destroy(&mut self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
        if let Ok(mut handles) = self.file_handles.lock() {
            handles.handles.clear();
            handles.next_fh = 1;
        }
        if let Ok(mut table) = self.inode_table.write() {
            table.inodes.clear();
            table.path_to_ino.clear();
        }
    }

    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let path = match self.inode_table.read().unwrap().child_path(parent, name) {
            Some(p) => p,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        {
            let table = self.inode_table.read().unwrap();
            if let Some(&ino) = table.path_to_ino.get(&path)
                && let Some(inode) = table.get(ino)
                && !inode.is_metadata_expired()
            {
                reply.entry(&FUSE_TTL, &inode.attr, 0);
                return;
            }
        }

        let parent_path = match self.inode_table.read().unwrap().get_cloned(parent) {
            Some(inode) => inode.path,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let entries = if let Ok(mut cache) = self.cache.lock() {
            match cache.list_directory_cached(&parent_path, &self.api_client) {
                Ok(entries) => entries,
                Err(e) => {
                    reply.error(e.errno);
                    return;
                }
            }
        } else {
            match self.api_client.list_directory(&parent_path) {
                Ok(entries) => entries,
                Err(e) => {
                    reply.error(e.errno);
                    return;
                }
            }
        };

        for entry in entries {
            if entry.name == name.to_string_lossy() {
                let full_path = if parent_path == "/" {
                    format!("/{}", entry.name)
                } else {
                    format!("{}/{}", parent_path, entry.name)
                };

                let ino = self.inode_table.write().unwrap().get_or_create(&full_path, &entry);
                if let Some(inode) = self.inode_table.read().unwrap().get_cloned(ino) {
                    reply.entry(&FUSE_TTL, &inode.attr, 0);
                    return;
                }
            }
        }

        reply.error(ENOENT);
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        if ino == 1
            && let Ok(mut cache) = self.cache.lock()
        {
            cache.cleanup_expired();
        }

        match self.inode_table.read().unwrap().get_cloned(ino) {
            Some(inode) => {
                let _ = inode.ino;
                reply.attr(&FUSE_TTL, &inode.attr)
            }
            None => reply.error(ENOENT),
        }
    }

    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<fuser::TimeOrNow>,
        _mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let inode = match self.inode_table.read().unwrap().get_cloned(ino) {
            Some(inode) => inode,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let path_lock = self.path_locks.get_lock(&inode.path);
        let _guard = path_lock.lock().unwrap();

        if let Some(new_size) = size
            && inode.attr.kind == FileType::RegularFile
        {
            let mut file_data = self.api_client.read_file(&inode.path).unwrap_or_default();
            file_data.resize(new_size as usize, 0);

            if let Err(e) = self.api_client.write_file(&inode.path, &file_data) {
                reply.error(e.errno);
                return;
            }

            self.invalidate_all_for_path(&inode.path);
            if let Some(node) = self.inode_table.write().unwrap().get_mut(ino) {
                node.attr.size = new_size;
                node.attr.mtime = SystemTime::now();
                node.cached_at = Instant::now();
            }
        }

        if let Some(new_mode) = mode {
            if let Err(e) = self.api_client.set_attrs(&inode.path, Some(new_mode & 0o777)) {
                reply.error(e.errno);
                return;
            }

            self.invalidate_all_for_path(&inode.path);
            if let Some(node) = self.inode_table.write().unwrap().get_mut(ino) {
                node.attr.perm = (new_mode & 0o777) as u16;
                node.attr.ctime = SystemTime::now();
                node.cached_at = Instant::now();
            }
        }

        match self.inode_table.read().unwrap().get_cloned(ino) {
            Some(inode) => reply.attr(&FUSE_TTL, &inode.attr),
            None => reply.error(ENOENT),
        }
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let inode = match self.inode_table.read().unwrap().get_cloned(ino) {
            Some(inode) => inode,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let entries = if let Ok(mut cache) = self.cache.lock() {
            match cache.list_directory_cached(&inode.path, &self.api_client) {
                Ok(entries) => entries,
                Err(e) => {
                    reply.error(e.errno);
                    return;
                }
            }
        } else {
            match self.api_client.list_directory(&inode.path) {
                Ok(entries) => entries,
                Err(e) => {
                    reply.error(e.errno);
                    return;
                }
            }
        };

        let mut i = offset;
        if i == 0 {
            if reply.add(ino, i + 1, FileType::Directory, ".") {
                reply.ok();
                return;
            }
            i += 1;
        }

        if i == 1 {
            if reply.add(ino, i + 1, FileType::Directory, "..") {
                reply.ok();
                return;
            }
            i += 1;
        }

        let mut table = self.inode_table.write().unwrap();
        for entry in entries.iter().skip((i - 2).max(0) as usize) {
            let full_path = if inode.path == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", inode.path, entry.name)
            };

            let entry_ino = table.get_or_create(&full_path, entry);
            let kind = if entry.is_dir {
                FileType::Directory
            } else {
                FileType::RegularFile
            };

            if reply.add(entry_ino, i + 1, kind, &entry.name) {
                break;
            }
            i += 1;
        }

        reply.ok();
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: fuser::ReplyOpen) {
        let _ = flags;
        let path = match self.inode_table.read().unwrap().get_cloned(ino) {
            Some(inode) => inode.path,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let fh = self.file_handles.lock().unwrap().open(path);
        reply.opened(fh, 0);
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.file_handles.lock().unwrap().close(fh);
        reply.ok();
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let inode = match self.inode_table.read().unwrap().get_cloned(ino) {
            Some(inode) => inode,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        if offset < 0 || offset as u64 >= inode.attr.size {
            reply.data(&[]);
            return;
        }

        let offset_u64 = offset as u64;
        let data = if let Ok(mut cache) = self.cache.lock() {
            match cache.read_with_cache(&inode.path, offset_u64, size, &self.api_client) {
                Ok(d) => d,
                Err(e) => {
                    reply.error(e.errno);
                    return;
                }
            }
        } else {
            match self.api_client.read_file_chunk(&inode.path, offset_u64, size) {
                Ok(d) => d,
                Err(e) => {
                    reply.error(e.errno);
                    return;
                }
            }
        };

        reply.data(&data);
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        let inode = match self.inode_table.read().unwrap().get_cloned(ino) {
            Some(inode) => inode,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let path_lock = self.path_locks.get_lock(&inode.path);
        let _guard = path_lock.lock().unwrap();

        match self.api_client.write_file_chunk(&inode.path, offset as u64, data) {
            Ok(_) => {
                self.invalidate_all_for_path(&inode.path);
                if let Some(node) = self.inode_table.write().unwrap().get_mut(ino) {
                    let new_size = ((offset as u64) + (data.len() as u64)).max(node.attr.size);
                    node.attr.size = new_size;
                    node.attr.mtime = SystemTime::now();
                    node.cached_at = Instant::now();
                }
                reply.written(data.len() as u32);
            }
            Err(e) => reply.error(e.errno),
        }
    }

    fn mkdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let path = match self.inode_table.read().unwrap().child_path(parent, name) {
            Some(p) => p,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let path_lock = self.path_locks.get_lock(&path);
        let _guard = path_lock.lock().unwrap();

        match self.api_client.create_directory(&path) {
            Ok(_) => {
                self.cache.lock().unwrap().invalidate_directory_cache(&path);

                let dir_mode = (if mode != 0 { mode & 0o777 } else { 0o755 }) | 0o700;
                let now_secs = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                let entry = FileEntry {
                    name: name.to_string_lossy().to_string(),
                    is_dir: true,
                    size: 0,
                    mtime: now_secs,
                    ctime: now_secs,
                    mode: dir_mode,
                };

                let ino = self.inode_table.write().unwrap().get_or_create(&path, &entry);
                if let Some(inode) = self.inode_table.read().unwrap().get_cloned(ino) {
                    reply.entry(&FUSE_TTL, &inode.attr, 0);
                } else {
                    reply.error(libc::EIO);
                }
            }
            Err(e) => reply.error(e.errno),
        }
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let path = match self.inode_table.read().unwrap().child_path(parent, name) {
            Some(p) => p,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let path_lock = self.path_locks.get_lock(&path);
        let _guard = path_lock.lock().unwrap();

        match self.api_client.delete(&path) {
            Ok(_) => {
                self.invalidate_all_for_path(&path);
                self.inode_table.write().unwrap().remove_by_path(&path);
                reply.ok();
            }
            Err(e) => reply.error(e.errno),
        }
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        self.unlink(_req, parent, name, reply);
    }

    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        let (from_path, to_path) = {
            let table = self.inode_table.read().unwrap();
            let from = match table.child_path(parent, name) {
                Some(p) => p,
                None => {
                    reply.error(ENOENT);
                    return;
                }
            };
            let to = match table.child_path(newparent, newname) {
                Some(p) => p,
                None => {
                    reply.error(ENOENT);
                    return;
                }
            };
            (from, to)
        };

        if from_path == to_path {
            reply.ok();
            return;
        }

        let (first_path, second_path) = if from_path <= to_path {
            (from_path.as_str(), to_path.as_str())
        } else {
            (to_path.as_str(), from_path.as_str())
        };
        let first_lock = self.path_locks.get_lock(first_path);
        let second_lock = self.path_locks.get_lock(second_path);
        let _first_guard = first_lock.lock().unwrap();
        let _second_guard = second_lock.lock().unwrap();

        match self.api_client.rename(&from_path, &to_path) {
            Ok(_) => {
                self.invalidate_all_for_path(&from_path);
                if let Some((parent_path, _)) = to_path.rsplit_once('/') {
                    let to_parent = if parent_path.is_empty() {
                        "/"
                    } else {
                        parent_path
                    };
                    self.cache.lock().unwrap().invalidate_directory_cache(to_parent);
                }
                self.invalidate_all_for_path(&to_path);
                self.inode_table.write().unwrap().rename(&from_path, &to_path);
                reply.ok();
            }
            Err(e) => reply.error(e.errno),
        }
    }

    fn create(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let path = match self.inode_table.read().unwrap().child_path(parent, name) {
            Some(p) => p,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let path_lock = self.path_locks.get_lock(&path);
        let _guard = path_lock.lock().unwrap();

        match self.api_client.write_file(&path, &[]) {
            Ok(_) => {
                self.cache.lock().unwrap().invalidate_directory_cache(&path);

                let file_mode = (if mode != 0 { mode & 0o777 } else { 0o644 }) | 0o600;
                let now_secs = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                let entry = FileEntry {
                    name: name.to_string_lossy().to_string(),
                    is_dir: false,
                    size: 0,
                    mtime: now_secs,
                    ctime: now_secs,
                    mode: file_mode,
                };

                let ino = self.inode_table.write().unwrap().get_or_create(&path, &entry);
                if let Some(inode) = self.inode_table.read().unwrap().get_cloned(ino) {
                    let fh = self.file_handles.lock().unwrap().open(path);
                    reply.created(&FUSE_TTL, &inode.attr, 0, fh, 0);
                } else {
                    reply.error(libc::EIO);
                }
            }
            Err(e) => reply.error(e.errno),
        }
    }
}

pub async fn run_fuser_client(api_client: ApiClient, mountpoint: String) -> Result<()> {
    let mountpoint_path = std::path::PathBuf::from(&mountpoint);

    #[cfg(target_os = "linux")]
    let unmount_hint = format!("fusermount -u {}", mountpoint_path.display());
    #[cfg(target_os = "macos")]
    let unmount_hint = format!("umount {}", mountpoint_path.display());

    log::info!("Mounting filesystem...");
    log::info!("Use Ctrl+C or '{}' to unmount", unmount_hint);

    let fs = RemoteFS::new(api_client);
    let unmounter: Arc<Mutex<Option<SessionUnmounter>>> = Arc::new(Mutex::new(None));
    let unmounter_for_signal = Arc::clone(&unmounter);

    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok()
            && let Ok(mut guard) = unmounter_for_signal.lock()
            && let Some(ref mut u) = *guard
        {
            let _ = u.unmount();
        }
    });

    let unmounter_for_mount = Arc::clone(&unmounter);
    tokio::task::spawn_blocking(move || {
        fs.mount(&mountpoint_path, unmounter_for_mount)
            .context("Failed to mount filesystem")
    })
    .await
    .context("Mount task panicked")??;

    Ok(())
}
