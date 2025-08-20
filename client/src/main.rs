use std::collections::HashMap;
use std::env;
use std::ffi::OsStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use fuser::{
    Filesystem, FileAttr, FileType, MountOption, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyEmpty, ReplyEntry, ReplyWrite, Request,
};
use libc::{EIO, ENOENT};
use reqwest::blocking::Client;
use serde::Deserialize;

const TTL: Duration = Duration::from_secs(1);

#[derive(Deserialize, Debug, Clone)]
struct RemoteEntry {
    name: String,
    is_dir: bool,
    size: u64,
    mtime: u64,
}

#[derive(Clone)]
struct InodeEntry {
    ino: u64,
    parent: u64,
    name: String,
    is_dir: bool,
    size: u64,
    mtime: u64,
    full_path: String,
}

fn default_file_attr(ino: u64, is_dir: bool, size: u64, mtime: u64) -> FileAttr {
    FileAttr {
        ino,
        size,
        blocks: 1,
        atime: UNIX_EPOCH + Duration::from_secs(mtime),
        mtime: UNIX_EPOCH + Duration::from_secs(mtime),
        ctime: UNIX_EPOCH + Duration::from_secs(mtime),
        crtime: UNIX_EPOCH + Duration::from_secs(mtime),
        kind: if is_dir {
            FileType::Directory
        } else {
            FileType::RegularFile
        },
        perm: if is_dir { 0o755 } else { 0o644 },
        nlink: 1,
        uid: 1000,
        gid: 1000,
        rdev: 0,
        flags: 0,
        blksize: 512,
    }
}

struct RemoteFS {
    server_url: String,
    client: Client,
    inode_map: Arc<Mutex<HashMap<u64, InodeEntry>>>,
    path_map: Arc<Mutex<HashMap<String, u64>>>,
    next_ino: Arc<Mutex<u64>>,
}

impl RemoteFS {
    fn get_inode(&self, path: &str) -> Option<u64> {
        self.path_map.lock().unwrap().get(path).copied()
    }
    fn add_inode(&self, entry: InodeEntry) -> u64 {
        let mut next_ino = self.next_ino.lock().unwrap();
        let ino = *next_ino;
        *next_ino += 1;
        self.inode_map.lock().unwrap().insert(ino, entry.clone());
        self.path_map.lock().unwrap().insert(entry.full_path.clone(), ino);
        ino
    }
}

impl Filesystem for RemoteFS {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name_str = name.to_str().unwrap_or("");
        let parent_entry = self.inode_map.lock().unwrap().get(&parent).cloned();
        let parent_path = if let Some(e) = parent_entry {
            e.full_path
        } else {
            "/".to_string()
        };
        let full_path = if parent_path == "/" {
            format!("/{}", name_str)
        } else {
            format!("{}/{}", parent_path, name_str)
        };

        let url = format!("{}/list{}", self.server_url, parent_path);
        let resp = self.client.get(&url).send();
        if let Ok(response) = resp {
            if let Ok(json) = response.json::<serde_json::Value>() {
                if let Some(contents) = json.get("contents").and_then(|c| c.as_array()) {
                    for entry in contents {
                        let entry_name = entry.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        if entry_name == name_str {
                            let is_dir = entry.get("is_dir").and_then(|d| d.as_bool()).unwrap_or(false);
                            let size = entry.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
                            let mtime = entry.get("mtime").and_then(|m| m.as_u64()).unwrap_or(0);
                            let inode_entry = InodeEntry {
                                ino: 0, // will be set by add_inode
                                parent,
                                name: entry_name.to_string(),
                                is_dir,
                                size,
                                mtime,
                                full_path: full_path.clone(),
                            };
                            let ino = self.add_inode(inode_entry);
                            let attr = default_file_attr(ino, is_dir, size, mtime);
                            reply.entry(&TTL, &attr, 0);
                            return;
                        }
                    }
                }
            }
        }
        reply.error(ENOENT);
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        if let Some(entry) = self.inode_map.lock().unwrap().get(&ino) {
            let attr = default_file_attr(ino, entry.is_dir, entry.size, entry.mtime);
            reply.attr(&TTL, &attr);
        } else if ino == 1 {
            // root
            let attr = default_file_attr(1, true, 0, 0);
            reply.attr(&TTL, &attr);
        } else {
            reply.error(ENOENT);
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
        let entry = self.inode_map.lock().unwrap().get(&ino).cloned();
        let dir_path = if let Some(e) = entry {
            e.full_path
        } else if ino == 1 {
            "/".to_string()
        } else {
            reply.error(ENOENT);
            return;
        };

        let url = format!("{}/list{}", self.server_url, dir_path);
        let resp = self.client.get(&url).send();

        if let Ok(response) = resp {
            if let Ok(json) = response.json::<serde_json::Value>() {
                if let Some(contents) = json.get("contents").and_then(|c| c.as_array()) {
                    let mut off = offset + 1;
                    reply.add(ino, off, FileType::Directory, ".");
                    off += 1;
                    reply.add(1, off, FileType::Directory, "..");
                    for (i, entry) in contents.iter().enumerate() {
                        let name = entry.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                        let is_dir = entry.get("is_dir").and_then(|d| d.as_bool()).unwrap_or(false);
                        let size = entry.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
                        let mtime = entry.get("mtime").and_then(|m| m.as_u64()).unwrap_or(0);
                        let full_path = if dir_path == "/" {
                            format!("/{}", name)
                        } else {
                            format!("{}/{}", dir_path, name)
                        };
                        let inode_entry = InodeEntry {
                            ino: 0,
                            parent: ino,
                            name: name.to_string(),
                            is_dir,
                            size,
                            mtime,
                            full_path: full_path.clone(),
                        };
                        let child_ino = self.add_inode(inode_entry);
                        let file_type = if is_dir { FileType::Directory } else { FileType::RegularFile };
                        reply.add(child_ino, off + (i as i64) + 1, file_type, name);
                    }
                    reply.ok();
                    return;
                }
            }
        }
        reply.error(ENOENT);
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        reply: ReplyData,
    ) {
        let entry = self.inode_map.lock().unwrap().get(&ino).cloned();
        if let Some(e) = entry {
            if e.is_dir {
                reply.error(EIO);
                return;
            }
            let url = format!("{}/files{}", self.server_url, e.full_path);
            let resp = self.client.get(&url).send();
            if let Ok(response) = resp {
                if let Ok(bytes) = response.bytes() {
                    let data = &bytes[offset as usize..std::cmp::min(bytes.len(), (offset as usize) + (size as usize))];
                    reply.data(data);
                    return;
                }
            }
        }
        reply.error(ENOENT);
    }

    // Scrittura file
    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _flags: u32,
        reply: ReplyWrite,
    ) {
        let entry = self.inode_map.lock().unwrap().get(&ino).cloned();
        if let Some(e) = entry {
            if e.is_dir {
                reply.error(EIO);
                return;
            }
            let url = format!("{}/files{}", self.server_url, e.full_path);
            // Per semplicità, scriviamo sempre tutto il file (no offset)
            let resp = self.client.put(&url).body(data.to_vec()).send();
            if let Ok(response) = resp {
                if response.status().is_success() {
                    reply.written(data.len() as u32);
                    return;
                }
            }
        }
        reply.error(EIO);
    }

    // Creazione file
    fn create(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _flags: u32,
        reply: ReplyCreate,
    ) {
        let parent_entry = self.inode_map.lock().unwrap().get(&parent).cloned();
        let parent_path = if let Some(e) = parent_entry {
            e.full_path
        } else {
            "/".to_string()
        };
        let full_path = if parent_path == "/" {
            format!("/{}", name.to_str().unwrap_or(""))
        } else {
            format!("{}/{}", parent_path, name.to_str().unwrap_or(""))
        };
        let url = format!("{}/files{}", self.server_url, full_path);
        let resp = self.client.put(&url).body(Vec::new()).send();
        if let Ok(response) = resp {
            if response.status().is_success() {
                // Aggiorna inode map
                let inode_entry = InodeEntry {
                    ino: 0,
                    parent,
                    name: name.to_str().unwrap_or("").to_string(),
                    is_dir: false,
                    size: 0,
                    mtime: 0,
                    full_path: full_path.clone(),
                };
                let ino = self.add_inode(inode_entry);
                let attr = default_file_attr(ino, false, 0, 0);
                reply.created(&TTL, &attr, 0, 0, 0);
                return;
            }
        }
        reply.error(EIO);
    }

    // Creazione directory
    fn mkdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        reply: ReplyEntry,
    ) {
        let parent_entry = self.inode_map.lock().unwrap().get(&parent).cloned();
        let parent_path = if let Some(e) = parent_entry {
            e.full_path
        } else {
            "/".to_string()
        };
        let full_path = if parent_path == "/" {
            format!("/{}", name.to_str().unwrap_or(""))
        } else {
            format!("{}/{}", parent_path, name.to_str().unwrap_or(""))
        };
        let url = format!("{}/mkdir{}", self.server_url, full_path);
        let resp = self.client.post(&url).send();
        if let Ok(response) = resp {
            if response.status().is_success() {
                let inode_entry = InodeEntry {
                    ino: 0,
                    parent,
                    name: name.to_str().unwrap_or("").to_string(),
                    is_dir: true,
                    size: 0,
                    mtime: 0,
                    full_path: full_path.clone(),
                };
                let ino = self.add_inode(inode_entry);
                let attr = default_file_attr(ino, true, 0, 0);
                reply.entry(&TTL, &attr, 0);
                return;
            }
        }
        reply.error(EIO);
    }

    // Cancellazione file o directory
    fn unlink(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        reply: ReplyEmpty,
    ) {
        let parent_entry = self.inode_map.lock().unwrap().get(&parent).cloned();
        let parent_path = if let Some(e) = parent_entry {
            e.full_path
        } else {
            "/".to_string()
        };
        let full_path = if parent_path == "/" {
            format!("/{}", name.to_str().unwrap_or(""))
        } else {
            format!("{}/{}", parent_path, name.to_str().unwrap_or(""))
        };
        let url = format!("{}/files{}", self.server_url, full_path);
        let resp = self.client.delete(&url).send();
        if let Ok(response) = resp {
            if response.status().is_success() {
                reply.ok();
                return;
            }
        }
        reply.error(EIO);
    }

    fn rmdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        reply: ReplyEmpty,
    ) {
        self.unlink(_req, parent, name, reply)
    }
}

#[cfg(target_os = "linux")]
fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <mountpoint> <server_url>", args[0]);
        std::process::exit(1);
    }
    let mountpoint = &args[1];
    let server_url = &args[2];

    // Inizializza la root
    let mut inode_map = HashMap::new();
    let mut path_map = HashMap::new();
    let root_entry = InodeEntry {
        ino: 1,
        parent: 1,
        name: "".to_string(),
        is_dir: true,
        size: 0,
        mtime: 0,
        full_path: "/".to_string(),
    };
    inode_map.insert(1, root_entry.clone());
    path_map.insert("/".to_string(), 1);

    let fs = RemoteFS {
        server_url: server_url.clone(),
        client: Client::new(),
        inode_map: Arc::new(Mutex::new(inode_map)),
        path_map: Arc::new(Mutex::new(path_map)),
        next_ino: Arc::new(Mutex::new(2)),
    };

    println!("Mounting remote fs from {} at {}", server_url, mountpoint);
    fuser::mount2(fs, mountpoint, &[MountOption::FSName("remotefs".to_string())]).unwrap();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("Questo client è implementato solo per Linux/FUSE.");
}