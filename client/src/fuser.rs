use fuser::{
    Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner, MountOption, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, Request, WriteFlags, mount2
};
use std::ffi::OsStr;
use std::time::{Duration, SystemTime};
use std::sync::Arc;
use libc::ENOENT;
use crate::{RemoteFilesystem, FileInfo};

const TTL: Duration = Duration::from_secs(1);

pub struct FuseAdapter {
    pub fs: Arc<RemoteFilesystem>,
}

impl FuseAdapter {
    fn make_attr(&self, ino: INodeNo, info: &FileInfo) -> FileAttr {
        FileAttr {
            ino: ino.into(),
            size: info.size,
            blocks: (info.size + 511) / 512,
            atime: SystemTime::UNIX_EPOCH + Duration::from_secs(info.timestamp),
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(info.timestamp),
            ctime: SystemTime::UNIX_EPOCH + Duration::from_secs(info.timestamp),
            crtime: SystemTime::UNIX_EPOCH + Duration::from_secs(info.timestamp),
            kind: if info.file_type == "dir" { FileType::Directory } else { FileType::RegularFile },
            perm: u16::from_str_radix(&info.permissions, 8).unwrap_or(0o755),
            nlink: if info.file_type == "dir" { 2 } else { 1 },
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            flags: 0,
            blksize: 512,
        }
    }
}

impl Filesystem for FuseAdapter {
    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        self.fs.runtime_handle.block_on(async {
            if let Some(path) = self.fs.get_path_by_ino(ino.into()).await {
                match self.fs.get_stat(&path).await {
                    Ok((res_ino, info)) => reply.attr(&TTL, &self.make_attr(INodeNo(res_ino), &info)),
                    Err(e) => reply.error(Errno::from_i32(e)),
                }
            } else {
                reply.error(Errno::from_i32(ENOENT));
            }
        });
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name_str = name.to_string_lossy();
        self.fs.runtime_handle.block_on(async {
            if let Some(parent_path) = self.fs.get_path_by_ino(parent.into()).await {
                let full_path = if parent_path.is_empty() || parent_path == "/" {
                    name_str.to_string()
                } else {
                    format!("{}/{}", parent_path.trim_end_matches('/'), name_str)
                };

                match self.fs.get_stat(&full_path).await {
                    Ok((ino, info)) => reply.entry(&TTL, &self.make_attr(INodeNo(ino), &info), Generation(0)),
                    Err(_) => reply.error(Errno::from_i32(ENOENT)),
                }
            } else {
                reply.error(Errno::from_i32(ENOENT));
            }
        });
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        self.fs.runtime_handle.block_on(async {
            if let Some(path) = self.fs.get_path_by_ino(ino.into()).await {
                match self.fs.list_dir(&path).await {
                    Ok(entries) => {
                        let mut curr_offset = offset;
                        
                        if curr_offset == 0 {
                            if reply.add(ino, 1, FileType::Directory, ".") { return reply.ok(); }
                            curr_offset = 1;
                        }

                        if curr_offset == 1 {
                            let parent_ino = *self.fs.parent_map.read().await.get(&ino.into()).unwrap_or(&1);
                            
                            if reply.add(INodeNo(parent_ino), 2, FileType::Directory, "..") { return reply.ok(); }
                            
                            curr_offset = 2;
                        }

                        for (i, (f_ino, f_info)) in entries.into_iter().enumerate().skip((curr_offset - 2) as usize) {
                            self.fs.parent_map.write().await.insert(f_ino, ino.into());
                            
                            let kind = if f_info.file_type == "dir" { FileType::Directory } else { FileType::RegularFile };
                            let next_o = (i as u64) + 3;
                            
                            if reply.add(INodeNo(f_ino), (next_o as i64).try_into().unwrap(), kind, &f_info.name) {
                                break;
                            }
                        }

                        reply.ok();
                    }
                    Err(e) => reply.error(Errno::from_i32(e)),
                }
            } else {
                reply.error(Errno::from_i32(ENOENT));
            }
        });
    }

    fn read(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, size: u32, _flags: OpenFlags, _lock: Option<LockOwner>, reply: ReplyData) {
        self.fs.runtime_handle.block_on(async {
            if let Some(path) = self.fs.get_path_by_ino(ino.into()).await {
                match self.fs.read_file(&path, offset as u64, size).await {
                    Ok(data) => reply.data(&data),
                    Err(e) => reply.error(Errno::from_i32(e)),
                }
            } else {
                reply.error(Errno::from_i32(ENOENT));
            }
        });
    }

    fn write(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, data: &[u8], _write_flags: WriteFlags, _flags: OpenFlags, _lock: Option<LockOwner>, reply: ReplyWrite) {
        self.fs.runtime_handle.block_on(async {
            if let Some(path) = self.fs.get_path_by_ino(ino.into()).await {
                match self.fs.write_file(&path, offset as u64, data.to_vec()).await {
                    Ok(_) => reply.written(data.len() as u32),
                    Err(e) => reply.error(Errno::from_i32(e)),
                }
            } else {
                reply.error(Errno::from_i32(ENOENT));
            }
        });
    }

    fn create(&self, _req: &Request, parent: INodeNo, name: &OsStr, _mode: u32, _umask: u32, _flags: i32, reply: ReplyCreate) {
        let name_str: std::borrow::Cow<'_, str> = name.to_string_lossy();
        self.fs.runtime_handle.block_on(async {
            if let Some(parent_path) = self.fs.get_path_by_ino(parent.into()).await {
                let full_path = if parent_path.is_empty() {
                    name_str.to_string()
                } else {
                    format!("{}/{}", parent_path.trim_end_matches('/'), name_str)
                };

                match self.fs.write_file(&full_path, 0, vec![]).await {
                    Ok(_) => {
                        match self.fs.get_stat(&full_path).await {
                            Ok((ino, info)) => {
                                let attr = self.make_attr(INodeNo(ino), &info);
                                reply.created(&TTL, &attr, Generation(0), FileHandle(0), FopenFlags::empty());
                            }
                            Err(e) => reply.error(Errno::from_i32(e)),
                        }
                    }
                    Err(e) => reply.error(Errno::from_i32(e)),
                }
            } else {
                reply.error(Errno::from_i32(ENOENT));
            }
        });
    }

    fn open(&self, _req: &Request, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        reply.opened(FileHandle(0), FopenFlags::empty());
    }

    fn mkdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, _mode: u32, _umask: u32, reply: ReplyEntry) {
        let name_str = name.to_string_lossy();
        
        self.fs.runtime_handle.block_on(async {
            if let Some(parent_path) = self.fs.get_path_by_ino(parent.into()).await {
                let full_path = format!("{}/{}", parent_path.trim_end_matches('/'), name_str);
                
                match self.fs.create_dir(&full_path).await {
                    Ok(_) => {
                        let (ino, info) = self.fs.get_stat(&full_path).await.unwrap();
                        reply.entry(&TTL, &self.make_attr(INodeNo(ino), &info), Generation(0));
                    }
                    Err(e) => reply.error(Errno::from_i32(e)),
                }
            } else {
                reply.error(Errno::from_i32(ENOENT));
            }
        });
    }

    fn rename(&self, _req: &Request, parent: INodeNo, name: &OsStr, newparent: INodeNo, newname: &OsStr, _flags: RenameFlags, reply: ReplyEmpty) {
        let name_str = name.to_string_lossy();
        let newname_str = newname.to_string_lossy();

        self.fs.runtime_handle.block_on(async {
            let old_p = self.fs.get_path_by_ino(parent.into()).await;
            let new_p = self.fs.get_path_by_ino(newparent.into()).await;

            if let (Some(op), Some(np)) = (old_p, new_p) {
                let old_full = format!("{}/{}", op.trim_end_matches('/'), name_str);
                let new_full = format!("{}/{}", np.trim_end_matches('/'), newname_str);
                
                match self.fs.rename(&old_full, &new_full).await {
                    Ok(_) => reply.ok(),
                    Err(e) => reply.error(Errno::from_i32(e)),
                }
            } else {
                reply.error(Errno::from_i32(ENOENT));
            }
        });
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.remove_any(parent, name, reply);
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.remove_any(parent, name, reply);
    }
}

impl FuseAdapter {
    fn remove_any(&self, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let name_str = name.to_string_lossy();
        
        self.fs.runtime_handle.block_on(async {
            if let Some(parent_path) = self.fs.get_path_by_ino(parent.into()).await {
                let full_path = format!("{}/{}", parent_path.trim_end_matches('/'), name_str);
                
                match self.fs.delete_path(&full_path).await {
                    Ok(_) => reply.ok(),
                    Err(e) => reply.error(Errno::from_i32(e)),
                }
            } else {
                reply.error(Errno::from_i32(ENOENT));
            }
        });
    }
}

pub fn run_fuser_client(fs: Arc<RemoteFilesystem>, mountpoint: String) {
    let _ = std::fs::create_dir_all(&mountpoint);

    let mut options = Config::default();
    options.mount_options = vec![
        MountOption::RW,
        MountOption::FSName("remote-file-system".to_string()),
        MountOption::CUSTOM("auto_cache".to_string()),
    ];

    mount2(FuseAdapter { fs }, &mountpoint, &options).expect("Mount failed");
}