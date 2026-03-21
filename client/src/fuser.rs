struct ApiClient {
    server_url: reqwest::Url,
    client: reqwest::Client,
}

impl ApiClient {
    async fn fetch_info(&self, path: &str) -> Result<FileInfo, i32> {
        self.client.get(self.server_url.join(&format!("/stat/{}", path)).unwrap())
            .send().await
            .map_err(|_| EIO)?
            .json().await
            .map_err(|_| EIO)
    }
}

pub const FUSE_ROOT_ID: u64 = 1;
const TTL: Duration = Duration::from_secs(1);

impl Filesystem for RemoteFilesystem {
    fn open(&mut self, _req: &Request<'_>, ino: u64, _flags: i32, reply: fuser::ReplyOpen) {
        println!("FUSE: open ino={}", ino);
        reply.opened(ino, 0);
    }

    fn setattr(&mut self, req: &Request<'_>, ino: u64, mode: Option<u32>, _uid: Option<u32>, _gid: Option<u32>, size: Option<u64>, _atime: Option<fuser::TimeOrNow>, _mtime: Option<fuser::TimeOrNow>, _ctime: Option<SystemTime>, _fh: Option<u64>, _crtime: Option<SystemTime>, _chgtime: Option<SystemTime>, _bkuptime: Option<SystemTime>, _flags: Option<u32>, reply: ReplyAttr,) {
        let path = {
            let inode_cache = self.inode_cache.lock().unwrap();
            if let Some(p) = inode_cache.get(&ino) {
                p.clone()
            } else {
                reply.error(ENOENT);
                return;
            }
        };

        if let Some(new_size) = size {
            let url = self.server_url.join(&format!("/truncate/{}", path)).unwrap();
            let res = self.runtime.block_on(async {
                let client = reqwest::Client::new();
                let res = client
                    .post(url)
                    .json(&serde_json::json!({"size": new_size as i64}))
                    .send()
                    .await
                    .map_err(|_| EIO)?;
                Ok::<_, i32>(res.status())
            });

            match res {
                Ok(status) if status.is_success() => {}
                // Some shells trigger truncate before any write; if the file doesn't exist yet,
                // create an empty file first and retry.
                Ok(status) if status.as_u16() == 404 => {
                    let create_url = self
                        .server_url
                        .join(&format!("/files/{}?offset=0", path))
                        .unwrap();
                    let create_status = self.runtime.block_on(async {
                        let client = reqwest::Client::new();
                        let res = client
                            .put(create_url)
                            .header("Content-Type", "application/octet-stream")
                            .body(Vec::<u8>::new())
                            .send()
                            .await
                            .map_err(|_| EIO)?;
                        Ok::<_, i32>(res.status())
                    });

                    match create_status {
                        Ok(s) if s.is_success() => {}
                        _ => {
                            reply.error(ENOENT);
                            return;
                        }
                    }

                    let retry_url = self.server_url.join(&format!("/truncate/{}", path)).unwrap();
                    let retry = self.runtime.block_on(async {
                        let client = reqwest::Client::new();
                        let res = client
                            .post(retry_url)
                            .json(&serde_json::json!({"size": new_size as i64}))
                            .send()
                            .await
                            .map_err(|_| EIO)?;
                        Ok::<_, i32>(res.status())
                    });

                    match retry {
                        Ok(s) if s.is_success() => {}
                        Ok(s) if s.as_u16() == 404 => {
                            reply.error(ENOENT);
                            return;
                        }
                        _ => {
                            reply.error(EIO);
                            return;
                        }
                    }
                }
                _ => {
                    reply.error(EIO);
                    return;
                }
            }
        }

        let mut metadata_cache = self.metadata_cache.lock().unwrap();

        if let Some((_, file_info)) = metadata_cache.get_mut(&path) {
            if let Some(new_size) = size {
                file_info.size = new_size as usize;
            }

            if let Some(new_mode) = mode {
                file_info.permissions = format!("{:o}", new_mode);
            }

            let timestamp = UNIX_EPOCH + Duration::from_secs(file_info.timestamp);
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
                uid: req.uid(),
                gid: req.gid(),
                rdev: 0,
                flags: 0,
                blksize: 512,
            };

            reply.attr(&TTL, &attr);
        } else {
            reply.error(ENOENT);
        }
    }

    fn create(&mut self, req: &Request<'_>, parent: u64, name: &OsStr, _mode: u32, _umask: u32, _flags: i32, reply: fuser::ReplyCreate) {
        println!("FUSE: create parent={} name={}", parent, name.to_string_lossy());
        let parent_path = {
            let inode_cache = self.inode_cache.lock().unwrap();
            if let Some(p) = inode_cache.get(&parent) {
                p.clone()
            } else {
                reply.error(ENOENT);
                return;
            }
        };

        let file_name = name.to_str().unwrap();
        let path = if parent_path == "" || parent_path == "/" {
            file_name.to_string()
        } else {
            format!("{}/{}", parent_path, file_name)
        };

        let create_url = self
            .server_url
            .join(&format!("/files/{}?offset=0", path))
            .unwrap();
        let create_status = self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client
                .put(create_url)
                .header("Content-Type", "application/octet-stream")
                .body(Vec::<u8>::new())
                .send()
                .await
                .map_err(|_| EIO)?;
            Ok::<_, i32>(res.status())
        });

        match create_status {
            Ok(s) if s.is_success() => {}
            Ok(s) if s.as_u16() == 409 => {
                reply.error(EEXIST);
                return;
            }
            _ => {
                reply.error(EIO);
                return;
            }
        }

        let stat_url = self.server_url.join(&format!("/stat/{}", path)).unwrap();
        let file_info: FileInfo = match self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client.get(stat_url).send().await.map_err(|_| EIO)?;
            if res.status().as_u16() == 404 {
                return Err(ENOENT);
            }
            if !res.status().is_success() {
                return Err(EIO);
            }
            res.json().await.map_err(|_| EIO)
        }) {
            Ok(info) => info,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        let (ino, timestamp) = {
            let mut inode_cache = self.inode_cache.lock().unwrap();
            let mut metadata_cache = self.metadata_cache.lock().unwrap();
            let mut next_inode = self.next_inode.lock().unwrap();

            let ino = *next_inode;
            *next_inode += 1;

            metadata_cache.insert(path.clone(), (ino, file_info.clone()));
            inode_cache.insert(ino, path.clone());

            (ino, file_info.timestamp)
        };

        let ts = UNIX_EPOCH + Duration::from_secs(timestamp);
        let perm = parse_permissions(&file_info.permissions);
        let attr = FileAttr {
            ino,
            size: file_info.size as u64,
            blocks: (file_info.size as u64 + 511) / 512,
            atime: ts,
            mtime: ts,
            ctime: ts,
            crtime: ts,
            kind: FileType::RegularFile,
            perm,
            nlink: 1,
            uid: req.uid(),
            gid: req.gid(),
            rdev: 0,
            flags: 0,
            blksize: 512,
        };

		reply.created(&TTL, &attr, 0, ino, 0);
    }

    fn lookup(&mut self, req: &Request<'_>, parent: u64, name: &OsStr, reply: fuser::ReplyEntry) {
        println!("FUSE: lookup parent={} name={}", parent, name.to_string_lossy());
        let parent_path = {
            let inode_cache = self.inode_cache.lock().unwrap();
            if let Some(p) = inode_cache.get(&parent) {
                p.clone()
            } else {
                reply.error(ENOENT);
                return;
            }
        };

        let file_name = name.to_str().unwrap();
        let path = if parent_path == "" || parent_path == "/" {
            file_name.to_string()
        } else {
            format!("{}/{}", parent_path, file_name)
        };

        let cached = {
            let metadata_cache = self.metadata_cache.lock().unwrap();
            metadata_cache.get(&path).cloned()
        };

        let (ino, file_info) = if let Some((ino, info)) = cached {
            (ino, info)
        } else {
            let url = self.server_url.join(&format!("/stat/{}", path)).unwrap();
            let file_info: FileInfo = match self.runtime.block_on(async {
                let client = reqwest::Client::new();
                let res = client.get(url).send().await.map_err(|_| EIO)?;
                if res.status().as_u16() == 404 {
                    return Err(ENOENT);
                }
                if !res.status().is_success() {
                    return Err(EIO);
                }
                res.json().await.map_err(|_| EIO)
            }) {
                Ok(info) => info,
                Err(errno) => {
                    reply.error(errno);
                    return;
                }
            };

            let new_inode = {
                let mut inode_cache = self.inode_cache.lock().unwrap();
                let mut metadata_cache = self.metadata_cache.lock().unwrap();
                let mut next_inode = self.next_inode.lock().unwrap();

                let new_inode = *next_inode;
                *next_inode += 1;

                metadata_cache.insert(path.clone(), (new_inode, file_info.clone()));
                inode_cache.insert(new_inode, path.clone());
                new_inode
            };

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
            uid: req.uid(),
            gid: req.gid(),
            rdev: 0,
            flags: 0,
            blksize: 512,
        };

        reply.entry(&TTL, &attr, 0);
    }

    fn getattr(&mut self, req: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        println!("FUSE: getattr ino={}", ino);
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
                uid: req.uid(),
                gid: req.gid(),
                rdev: 0,
                flags: 0,
                blksize: 0,
            };
            
            reply.attr(&TTL, &attr);
            return;
        }

        let path = {
            let inode_cache = self.inode_cache.lock().unwrap();
            if let Some(p) = inode_cache.get(&ino) {
                p.clone()
            } else {
                reply.error(ENOENT);
                return;
            }
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
                uid: req.uid(),
                gid: req.gid(),
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
        println!("FUSE: readdir ino={} offset={}", ino, offset);
		
		let mut entries = vec![
            (FUSE_ROOT_ID, FileType::Directory, OsString::from(".")),
            (FUSE_ROOT_ID, FileType::Directory, OsString::from(".."))
        ];

        let path = {
            let inode_cache = self.inode_cache.lock().unwrap();
            if let Some(p) = inode_cache.get(&ino) {
                p.clone()
            } else {
                reply.error(ENOENT);
                return;
            }
        };

        let url = self.server_url.join(&format!("/list/{}", path)).unwrap();
        let files: Vec<FileInfo> = match self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client.get(url).send().await.map_err(|_| EIO)?;

            if res.status().as_u16() == 404 {
                return Err(ENOENT);
            }
            if !res.status().is_success() {
                return Err(EIO);
            }

            res.json().await.map_err(|_| EIO)
        }) {
            Ok(f) => f,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        let mut inode_cache = self.inode_cache.lock().unwrap();
        let mut metadata_cache = self.metadata_cache.lock().unwrap();
        for file in &files {
            let inode = if let Some((ino, _)) = metadata_cache.get(&file.path) {
                *ino
            } else {
                let mut next_inode = self.next_inode.lock().unwrap();
                let new_inode = *next_inode;
                *next_inode += 1;
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
        println!("FUSE: read ino={} offset={} size={}", ino, offset, size);
        
        let path = {
            let inode_cache = self.inode_cache.lock().unwrap();
            if let Some(p) = inode_cache.get(&ino) {
                p.clone()
            } else {
                reply.error(ENOENT);
                return;
            }
        };

        let url = self.server_url.join(&format!("/files/{}", path)).unwrap();
        let data = match self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client.get(url).send().await.map_err(|_| EIO)?;
            if res.status().as_u16() == 404 {
                return Err(ENOENT);
            }
            if !res.status().is_success() {
                return Err(EIO);
            }
            res.bytes().await.map_err(|_| EIO)
        }) {
            Ok(b) => b,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };


        let end = (offset + size as i64) as usize;
        let data_slice = if end > data.len() {
            &data[(offset as usize)..]
        } else {
            &data[(offset as usize)..end]
        };

        reply.data(data_slice);
    }

    fn write(&mut self, _req: &Request<'_>, ino: u64, _fh: u64, offset: i64, data: &[u8], _write_flags: u32, _flags: i32, _lock_owner: Option<u64>, reply: fuser::ReplyWrite) {
        println!("FUSE: write ino={} offset={} len={}", ino, offset, data.len());
        
        let path = {
            let inode_cache = self.inode_cache.lock().unwrap();
            if let Some(p) = inode_cache.get(&ino) {
                p.clone()
            } else {
                reply.error(ENOENT);
                return;
            }
        };

        if offset < 0 {
            reply.error(EIO);
            return;
        }

        let url = self
            .server_url
            .join(&format!("/files/{}?offset={}", path, offset))
            .unwrap();
        let res = match self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client
                .put(url)
                .header("Content-Type", "application/octet-stream")
                .body(data.to_vec())
                .send()
                .await
                .map_err(|_| EIO)?;
            Ok::<_, i32>(res.status())
        }) {
            Ok(s) => s,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        if res.is_success() {
            let url = self.server_url.join(&format!("/stat/{}", path)).unwrap();
            if let Ok(file_info) = self.runtime.block_on(async {
                let client = reqwest::Client::new();
                let res = client.get(url).send().await.map_err(|_| EIO)?;
                if !res.status().is_success() {
                    return Err(EIO);
                }
                res.json().await.map_err(|_| EIO)
            }) {
                let mut metadata_cache = self.metadata_cache.lock().unwrap();
                if let Some((_, info)) = metadata_cache.get_mut(&path) {
                    *info = file_info;
                }
            }
			
            reply.written(data.len() as u32);
        } else if res.as_u16() == 404 {
            reply.error(ENOENT);
        } else {
            reply.error(EIO);
        }
    }

    fn mkdir(&mut self, req: &Request<'_>, parent: u64, name: &OsStr, _mode: u32, _umask: u32, reply: fuser::ReplyEntry) {
        println!("FUSE: mkdir parent={} name={}", parent, name.to_string_lossy());
        
        let parent_path = {
            let inode_cache = self.inode_cache.lock().unwrap();
            if let Some(p) = inode_cache.get(&parent) {
                p.clone()
            } else {
                reply.error(ENOENT);
                return;
            }
        };

        let dir_name = name.to_str().unwrap();
        let path = if parent_path.is_empty() || parent_path == "/" {
            dir_name.to_string()
        } else {
            format!("{}/{}", parent_path, dir_name)
        };

        let url = self.server_url.join(&format!("/mkdir/{}", path)).unwrap();
        let res = self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client.post(url).send().await.unwrap();
            res.status()
        });

        if res.is_success() {
            let stat_url = self.server_url.join(&format!("/stat/{}", path)).unwrap();
            let file_info: FileInfo = match self.runtime.block_on(async {
                let client = reqwest::Client::new();
                let res = client.get(stat_url).send().await.map_err(|_| EIO)?;
                if res.status().as_u16() == 404 {
                    return Err(ENOENT);
                }
                if !res.status().is_success() {
                    return Err(EIO);
                }
                res.json().await.map_err(|_| EIO)
            }) {
                Ok(info) => info,
                Err(errno) => {
                    reply.error(errno);
                    return;
                }
            };

            let (ino, timestamp) = {
                let mut inode_cache = self.inode_cache.lock().unwrap();
                let mut metadata_cache = self.metadata_cache.lock().unwrap();
                let mut next_inode = self.next_inode.lock().unwrap();

                let ino = *next_inode;
                *next_inode += 1;

                metadata_cache.insert(path.clone(), (ino, file_info.clone()));
                inode_cache.insert(ino, path.clone());

                (ino, file_info.timestamp)
            };

            let ts = UNIX_EPOCH + Duration::from_secs(timestamp);
            let perm = parse_permissions(&file_info.permissions);
            let attr = FileAttr {
                ino,
                size: file_info.size as u64,
                blocks: (file_info.size as u64 + 511) / 512,
                atime: ts,
                mtime: ts,
                ctime: ts,
                crtime: ts,
                kind: FileType::Directory,
                perm,
                nlink: 2,
                uid: req.uid(),
                gid: req.gid(),
                rdev: 0,
                flags: 0,
                blksize: 512,
            };

            reply.entry(&TTL, &attr, 0);
        } else if res.as_u16() == 409 {
            reply.error(EEXIST);
        } else {
            reply.error(EIO);
        }
    }

    fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        println!("FUSE: unlink parent={} name={}", parent, name.to_string_lossy());
        
        let mut inode_cache = self.inode_cache.lock().unwrap();
        let parent_path = if let Some(p) = inode_cache.get(&parent) {
            p.clone()
        } else {
            reply.error(ENOENT);
            return;
        };

        let file_name = name.to_str().unwrap();
        let path = if parent_path.is_empty() || parent_path == "/" {
            file_name.to_string()
        } else {
            format!("{}/{}", parent_path, file_name)
        };

        let url = self.server_url.join(&format!("/files/{}", path)).unwrap();
        let res = self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client.delete(url).send().await.unwrap();
            
            res.status()
        });
        
        if res.is_success() {
            let mut metadata_cache = self.metadata_cache.lock().unwrap();
            
            if let Some((ino, _)) = metadata_cache.remove(&path) {
                inode_cache.remove(&ino);
            }

            reply.ok();
        } else if res.as_u16() == 404 {
            reply.error(ENOENT);
        } else {
            reply.error(EIO);
        }
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        println!("FUSE: rmdir parent={} name={}", parent, name.to_string_lossy());
        
        let mut inode_cache = self.inode_cache.lock().unwrap();
        let parent_path = if let Some(p) = inode_cache.get(&parent) {
            p.clone()
        } else {
            reply.error(ENOENT);
            return;
        };

        let dir_name = name.to_str().unwrap();
        let path = if parent_path.is_empty() || parent_path == "/" {
            dir_name.to_string()
        } else {
            format!("{}/{}", parent_path, dir_name)
        };

        let url = self.server_url.join(&format!("/files/{}", path)).unwrap();
        let res = self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client.delete(url).send().await.unwrap();
            
            res.status()
        });
        
        if res.is_success() {
            let mut metadata_cache = self.metadata_cache.lock().unwrap();
            
            if let Some((ino, _)) = metadata_cache.remove(&path) {
                inode_cache.remove(&ino);
            }

            reply.ok();
        } else if res.as_u16() == 404 {
            reply.error(ENOENT);
        } else {
            reply.error(EIO);
        }
    }

    fn rename(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, newparent: u64, newname: &OsStr, _flags: u32, reply: ReplyEmpty) {
        println!("FUSE: rename parent={} name={} -> newparent={} newname={}", parent, name.to_string_lossy(), newparent, newname.to_string_lossy());

        let old_name = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(EINVAL);
                return;
            }
        };
        let new_name = match newname.to_str() {
            Some(s) => s,
            None => {
                reply.error(EINVAL);
                return;
            }
        };

        let (parent_path, new_parent_path) = {
            let inode_cache = self.inode_cache.lock().unwrap();
            let parent_path = if let Some(p) = inode_cache.get(&parent) {
                p.clone()
            } else {
                reply.error(ENOENT);
                return;
            };
            let new_parent_path = if let Some(p) = inode_cache.get(&newparent) {
                p.clone()
            } else {
                reply.error(ENOENT);
                return;
            };
            (parent_path, new_parent_path)
        };

        let old_path = if parent_path.is_empty() || parent_path == "/" {
            old_name.to_string()
        } else {
            format!("{}/{}", parent_path, old_name)
        };

        let new_path = if new_parent_path.is_empty() || new_parent_path == "/" {
            new_name.to_string()
        } else {
            format!("{}/{}", new_parent_path, new_name)
        };

        let url = self.server_url.join("/rename").unwrap();
        let status = match self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client
                .post(url)
                .json(&serde_json::json!({"from": old_path, "to": new_path}))
                .send()
                .await
                .map_err(|_| EIO)?;
            Ok::<_, i32>(res.status())
        }) {
            Ok(s) => s,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        if !status.is_success() {
            match status.as_u16() {
                400 => reply.error(EINVAL),
                404 => reply.error(ENOENT),
                409 => reply.error(EEXIST),
                _ => reply.error(EIO),
            }
            return;
        }

        let mut inode_cache = self.inode_cache.lock().unwrap();
        let mut metadata_cache = self.metadata_cache.lock().unwrap();

        let old_prefix = format!("{}/", old_path);
        let new_prefix = format!("{}/", new_path);

        let keys: Vec<String> = metadata_cache.keys().cloned().collect();
        for key in keys {
            if key == old_path || key.starts_with(&old_prefix) {
                let new_key = if key == old_path {
                    new_path.clone()
                } else {
                    let rest = &key[old_prefix.len()..];
                    format!("{}{}", new_prefix, rest)
                };

                if let Some((ino, mut info)) = metadata_cache.remove(&key) {
                    info.path = new_key.clone();
                    info.name = new_key
                        .split('/')
                        .last()
                        .unwrap_or("")
                        .to_string();
                    let updated_path = info.path.clone();
                    metadata_cache.insert(new_key, (ino, info));
                    inode_cache.insert(ino, updated_path);
                }
            }
        }

        for(_ino, p) in inode_cache.iter_mut() {
            if *p == old_path {
                *p = new_path.clone();
            } else if p.starts_with(&old_prefix) {
                let rest = &p[old_prefix.len()..];
                *p = format!("{}{}", new_prefix, rest);
            }
        }

        reply.ok();
    }
}

pub fn run_fuser_client(filesystem: RemoteFilesystem) {
    let mountpoint = "mnt/remote-fs";
    println!("Mounting filesystem at {}", mountpoint); // TODO wait for server

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    
    ctrlc::set_handler(move || {
        println!("Received shutdown signal, unmounting...");
        r.store(false, Ordering::SeqCst);
    }).expect("Error setting Ctrl-C handler");

    let handle = thread::spawn(move || {
        let opts: [MountOption; 0] = [];

        println!("Mounting with options: {:?}", opts);

        let res = mount2(filesystem, mountpoint, &opts);
        if let Err(err) = res {
            println!("Error while mounting the filesystem: {}", err);
            process::exit(1);
        }
    });
    
    while running.load(Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(100));
    }
    
    #[cfg(target_os = "linux")]
    {
        let try_unmount = |bin: &str, args: &[&str]| -> bool {
            std::process::Command::new(bin)
                .args(args)
                .arg(mountpoint)
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };

        let unmounted = try_unmount("fusermount3", &["-u", "-z"])
            || try_unmount("fusermount", &["-u", "-z"])
            || std::process::Command::new("umount")
                .args(["-l", mountpoint])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);

        if !unmounted {
            eprintln!(
                "Warning: failed to unmount {} (it may still be busy). Try: fusermount3 -uz {}",
                mountpoint, mountpoint
            );
        }
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("umount")
        .arg(mountpoint)
        .status();

    println!("Exiting.");
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