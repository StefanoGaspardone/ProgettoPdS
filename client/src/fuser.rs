use fuser::{Filesystem, mount2, Request, ReplyAttr, ReplyData, ReplyDirectory, FileAttr, FileType, ReplyEmpty};
use libc::{ENOENT, EIO};
use std::ffi::{OsStr, OsString};
use std::{process, thread};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{RemoteFilesystem, FileInfo};

pub const FUSE_ROOT_ID: u64 = 1;
const TTL: Duration = Duration::from_secs(1);

impl Filesystem for RemoteFilesystem {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: fuser::ReplyEntry) {
        let inode_cache = self.inode_cache.lock().unwrap();
        let parent_path = if let Some(p) = inode_cache.get(&parent) {
            p.clone()
        } else {
            reply.error(ENOENT);
            return;
        };

        let file_name = name.to_str().unwrap();
        let path = if parent_path == "" || parent_path == "/" {
            file_name.to_string()
        } else {
            format!("{}/{}", parent_path, file_name)
        };

        let mut metadata_cache = self.metadata_cache.lock().unwrap();
        let (ino, file_info) = if let Some((ino, info)) = metadata_cache.get(&path) {
            (*ino, info.clone())
        } else {
            let url = self.server_url.join(&format!("/stat/{}", path)).unwrap();
            let file_info: FileInfo = match self.runtime.block_on(async {
                let client = reqwest::Client::new();
                let res = client.get(url).send().await?;
                res.json().await
            }) {
                Ok(info) => info,
                Err(_) => {
                    reply.error(ENOENT);
                    return;
                }
            };

            let new_inode = self.next_inode;
            self.next_inode += 1;

            metadata_cache.insert(path.clone(), (new_inode, file_info.clone()));
            
            let mut inode_cache = self.inode_cache.lock().unwrap();
            inode_cache.insert(new_inode, path.clone());

            (new_inode, file_info)
        };

        let timestamp_seconds = file_info.timestamp;
        let timestamp = UNIX_EPOCH + Duration::from_secs(timestamp_seconds);
        let perm = parse_permissions(&file_info.permissions);
        let kind = if file_info.file_type == "dir" { FileType::Directory } else { FileType::RegularFile };
        let attr = FileAttr {
            ino,
            size: file_info.size as u64,
            blocks: (file_info.size as u64 + 511) / 512,
            atime: timestamp,
            mtime: timestamp,
            ctime: timestamp,
            crtime: timestamp,
            kind,
            perm,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            flags: 0,
            blksize: 512,
        };
        reply.entry(&TTL, &attr, 0);
    }
    fn getattr(&mut self, _req: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        if ino == FUSE_ROOT_ID {
            let attr = FileAttr {
                ino: FUSE_ROOT_ID,
                size: 0,
                blocks: 0,
                atime: SystemTime::now(),
                mtime: SystemTime::now(),
                ctime: SystemTime::now(),
                crtime: SystemTime::now(),
                kind: FileType::Directory,
                perm: 0o755,
                nlink: 2,
                uid: 0,
                gid: 0,
                rdev: 0,
                flags: 0,
                blksize: 0,
            };
            
            reply.attr(&TTL, &attr);
            return;
        }

        let inode_cache = self.inode_cache.lock().unwrap();
        let path = if let Some(p) = inode_cache.get(&ino) {
            p.clone()
        } else {
            reply.error(ENOENT);
            return;
        };

        let metadata_cache = self.metadata_cache.lock().unwrap();
        if let Some((_, file_info)) = metadata_cache.get(&path) {
            let timestamp_seconds = file_info.timestamp;
            let timestamp = UNIX_EPOCH + Duration::from_secs(timestamp_seconds);
            
            let perm = parse_permissions(&file_info.permissions);
            
            let attr = FileAttr {
                ino,
                size: file_info.size as u64,
                blocks: (file_info.size as u64 + 511) / 512,
                atime: timestamp,
                mtime: timestamp,
                ctime: timestamp,
                crtime: timestamp,
                kind: if file_info.file_type == "dir" { FileType::Directory } else { FileType::RegularFile },
                perm,
                nlink: 1,
                uid: 0,
                gid: 0,
                rdev: 0,
                flags: 0,
                blksize: 512,
            };

            reply.attr(&TTL, &attr);
        } else {
            reply.error(ENOENT);
        }
    }

    fn readdir(&mut self, _req: &Request<'_>, ino: u64, _fh: u64, offset: i64, mut reply: ReplyDirectory) {
        // if ino != FUSE_ROOT_ID {
        //     reply.error(ENOENT);
        //     return;
        // }

        let mut entries = vec![
            (FUSE_ROOT_ID, FileType::Directory, OsString::from(".")),
            (FUSE_ROOT_ID, FileType::Directory, OsString::from(".."))
        ];

        let mut inode_cache = self.inode_cache.lock().unwrap();
        let path = if let Some(p) = inode_cache.get(&ino) {
            p.clone()
        } else {
            reply.error(ENOENT);
            return;
        };

        let url = self.server_url.join(&format!("/list/{}", path)).unwrap();
        let files: Vec<FileInfo> = match self.runtime.block_on(async {
            println!("Chiamo il server per /list/{}", path);
            let client = reqwest::Client::new();
            let res = client.get(url).send().await?;
            res.json().await
        }) {
            Ok(f) => f,
            Err(_) => {
                reply.error(EIO);
                return;
            }
        };

        let mut metadata_cache = self.metadata_cache.lock().unwrap();
        for file in &files {
            let inode = if let Some((ino, _)) = metadata_cache.get(&file.path) {
                *ino
            } else {
                let new_inode = self.next_inode;
                self.next_inode += 1;
                metadata_cache.insert(file.path.clone(), (new_inode, file.clone()));
                inode_cache.insert(new_inode, file.path.clone());
                new_inode
            };

            let kind = if file.file_type == "dir" { FileType::Directory } else { FileType::RegularFile };
            entries.push((inode, kind, OsString::from(file.name.clone())));
        }

        for (i, (ino, kind, name)) in entries.into_iter().enumerate().skip(offset as usize) {
            let _ = reply.add(ino, (i + 1) as i64, kind, name);
        }

        reply.ok();
    }

    fn read(&mut self, _req: &Request<'_>, ino: u64, _fh: u64, offset: i64, size: u32, _flags: i32, _lock_owner: Option<u64>, reply: ReplyData) {
        let inode_cache = self.inode_cache.lock().unwrap();
        let path = if let Some(p) = inode_cache.get(&ino) {
            p.clone()
        } else {
            reply.error(ENOENT);
            return;
        };

        let url = self.server_url.join(&format!("/files/{}", path)).unwrap();
        let data = self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client.get(url).send().await.unwrap();
            res.bytes().await.unwrap()
        });


        let end = (offset + size as i64) as usize;
        let data_slice = if end > data.len() {
            &data[(offset as usize)..]
        } else {
            &data[(offset as usize)..end]
        };

        reply.data(data_slice);
    }

    fn write(&mut self, _req: &Request<'_>, ino: u64, _fh: u64, _offset: i64, data: &[u8], _write_flags: u32, _flags: i32, _lock_owner: Option<u64>, reply: fuser::ReplyWrite) {
        let inode_cache = self.inode_cache.lock().unwrap();
        let path = if let Some(p) = inode_cache.get(&ino) {
            p.clone()
        } else {
            reply.error(ENOENT);
            return;
        };

        let url = self.server_url.join(&format!("/files/{}", path)).unwrap();
        let res = self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client.put(url).body(data.to_vec()).send().await.unwrap();
            res.status()
        });

        if res.is_success() {
            reply.written(data.len() as u32);
        } else {
            reply.error(EIO);
        }
    }

    fn mkdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, mode: u32, _umask: u32, reply: fuser::ReplyEntry) {
        let inode_cache = self.inode_cache.lock().unwrap();
        let parent_path = if let Some(p) = inode_cache.get(&parent) {
            p.clone()
        } else {
            reply.error(ENOENT);
            return;
        };

        let dir_name = name.to_str().unwrap();
        let path = format!("{}/{}", parent_path, dir_name);

        let url = self.server_url.join(&format!("/mkdir/{}", path)).unwrap();
        let res = self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client.post(url).send().await.unwrap();
            res.status()
        });

        if res.is_success() {
            let attr = FileAttr {
                ino: self.next_inode,
                size: 0,
                blocks: 0,
                atime: SystemTime::now(),
                mtime: SystemTime::now(),
                ctime: SystemTime::now(),
                crtime: SystemTime::now(),
                kind: FileType::Directory,
                perm: mode as u16,
                nlink: 2,
                uid: 0,
                gid: 0,
                rdev: 0,
                flags: 0,
                blksize: 512,
            };

            let mut metadata_cache = self.metadata_cache.lock().unwrap();
            let mut inode_cache = self.inode_cache.lock().unwrap();
            let file_info = FileInfo {
                name: dir_name.to_string(),
                path: path.clone(),
                file_type: "dir".to_string(),
                size: 0,
                timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
                permissions: format!("{:o}", mode),
            };
            metadata_cache.insert(path.clone(), (self.next_inode, file_info));
            inode_cache.insert(self.next_inode, path.clone());

            self.next_inode += 1;
            
            reply.entry(&TTL, &attr, 0);
        } else {
            reply.error(EIO);
        }
    }

    fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let inode_cache = self.inode_cache.lock().unwrap();
        let parent_path = if let Some(p) = inode_cache.get(&parent) {
            p.clone()
        } else {
            reply.error(ENOENT);
            return;
        };

        let file_name = name.to_str().unwrap();
        let path = format!("{}/{}", parent_path, file_name);

        let url = self.server_url.join(&format!("/files/{}", path)).unwrap();
        let res = self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client.delete(url).send().await.unwrap();
            res.status()
        });
        
        if res.is_success() {
            let mut metadata_cache = self.metadata_cache.lock().unwrap();
            let mut inode_cache = self.inode_cache.lock().unwrap();

            if let Some((ino, _)) = metadata_cache.remove(&path) {
                inode_cache.remove(&ino);
            }

            reply.ok();
        } else {
            reply.error(EIO);
        }
    }
}

pub fn run_fuser_client(filesystem: RemoteFilesystem) {
    let mountpoint = "mnt/remote-fs";
    println!("Mounting filesystem at {}", mountpoint);

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    
    ctrlc::set_handler(move || {
        println!("Received shutdown signal, unmounting...");
        r.store(false, Ordering::SeqCst);
    }).expect("Error setting Ctrl-C handler");

    let handle = thread::spawn(move || {
        let res = mount2(filesystem, mountpoint, &[]);
        if let Err(err) = res {
            println!("Error while mounting the filesystem: {}", err);
            process::exit(1);
        }
    });
    
    while running.load(Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(100));
    }
    
    let _ = std::process::Command::new("fusermount")
        .arg("-u")
        .arg(mountpoint)
        .status();

    println!("Filesystem unmounted, exiting.");
    let _ = handle.join();
}

fn parse_permissions(perm_str: &str) -> u16 {
    let mut perm = 0;
    let chars: Vec<char> = perm_str.chars().collect();

    if chars.len() == 9 {
        if chars[0] == 'r' { perm |= 0o400; }
        if chars[1] == 'w' { perm |= 0o200; }
        if chars[2] == 'x' { perm |= 0o100; }
        if chars[3] == 'r' { perm |= 0o040; }
        if chars[4] == 'w' { perm |= 0o020; }
        if chars[5] == 'x' { perm |= 0o010; }
        if chars[6] == 'r' { perm |= 0o004; }
        if chars[7] == 'w' { perm |= 0o002; }
        if chars[8] == 'x' { perm |= 0o001; }
    }

    perm
}