use fuser::{Filesystem, mount2, FUSE_ROOT_ID, Request, ReplyAttr, ReplyData, ReplyDirectory, FileAttr, FileType, ReplyEmpty};
use libc::{ENOENT, EIO};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::os::unix::prelude::OsStringExt;
use std::time::{self, Duration, SystemTime, UNIX_EPOCH};
use std::path::PathBuf;
use std::io::ErrorKind;
use bytes::Bytes;
use reqwest::StatusCode;

use crate::{RemoteFilesystem, FileInfo};


const TTL: Duration = Duration::from_secs(1);

impl Filesystem for RemoteFilesystem {
    fn getattr(&mut self, _req: &Request<'_>, ino: u64, fh: Option<u64>, reply: ReplyAttr) {
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
            let timestamp_seconds = file_info.timestamp.parse::<u64>().unwrap_or(0);
            let timestamp = UNIX_EPOCH + Duration::from_secs(timestamp_seconds);

            // TODO implementare logica per conversione dei permessi
            let perm = if file_info.file_type == "dir" { 0o755 } else { 0o644 };
            
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

    fn readdir(&mut self, _req: &Request<'_>, ino: u64, fh: u64, offset: i64, reply: ReplyDirectory) {
        if ino != FUSE_ROOT_ID {
            reply.error(ENOENT);
            return;
        }

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
            reqwest::get(url).await?.json().await
        }) {
            Ok(f) => f,
            Err(_) => {
                reply.error(EIO);
                return;
            }
        };

        let mut metadata_cache = self.metadata_cache.lock().unwrap();
        for file in files {
            let new_inode = self.next_inode;
            self.next_inode += 1;

            metadata_cache.insert(file.path.clone(), (new_inode, file));
            inode_cache.insert(new_inode, file.path.clone());

            let kind = if file.file_type == "dir" { FileType::Directory } else { FileType::RegularFile };
            entries.push((new_inode, kind, OsString::from(file.name)));
        }

        for (i, (ino, kind, name)) in entries.into_iter().enumerate().skip(offset as usize) {
            reply.add(ino, (i + 1) as i64, kind, name);
        }

        reply.ok();
    }

    fn read(&mut self, _req: &Request<'_>, ino: u64, fh: u64, offset: i64, size: u32, flags: i32, lock_owner: Option<u64>, reply: ReplyData) {
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

    fn write(&mut self, _req: &Request<'_>, ino: u64, fh: u64, offset: i64, data: &[u8], write_flags: u32, flags: i32, lock_owner: Option<u64>, reply: fuser::ReplyWrite) {
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

    fn mkdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, mode: u32, umask: u32, reply: fuser::ReplyEntry) {
        let inode_cache = self.inode_cache.lock().unwrap();
        let parent_path = if let Some(p) = inode_cache.get(&parent) {
            p.clone()
        } else {
            reply.error(ENOENT);
            return;
        };

        let dir_name = name.to_str().unwrap();
        let path = format!("{}/{}", parent_path,sssssssssss);
    }
}