// Questo file è quasi identico al codice precedente, ma refattorizzato
// per usare RemoteApi.

use crate::remote_api::RemoteApi;
use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    Request,
};
use libc::ENOENT;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(1);
const FUSE_ROOT_INO: u64 = 1;

pub struct RemoteFS {
    api: RemoteApi,
    paths: Arc<Mutex<HashMap<u64, PathBuf>>>,
    next_ino: Arc<Mutex<u64>>,
}

// ... (tutte le funzioni helper come get_next_ino, add_path, ecc.)
// ... (l'implementazione completa del trait `impl Filesystem for RemoteFS`)
// Per brevità, non riporto tutto il codice che è quasi identico,
// ma l'idea chiave è sostituire `self.client.get(...)` con `self.api.list_directory(...)` ecc.

// Funzione di avvio per FUSE
pub fn mount(mount_point: &str, remote_url: String) {
    let mut paths = HashMap::new();
    paths.insert(FUSE_ROOT_INO, PathBuf::from("/"));

    let filesystem = RemoteFS {
        api: RemoteApi::new(remote_url),
        paths: Arc::new(Mutex::new(paths)),
        next_ino: Arc::new(Mutex::new(FUSE_ROOT_INO)),
    };

    let mount_options = vec![
        MountOption::FSName("remote-fs".to_string()),
        MountOption::AutoUnmount,
        MountOption::AllowRoot,
    ];

    println!("Mounting remote filesystem at {}", mount_point);
    fuser::mount2(filesystem, mount_point, &mount_options).unwrap();
    println!("Filesystem unmounted.");
}


// --- INCOLLA QUI IL RESTO DEL CODICE FUSE ---
// Sostituisci la vecchia logica `reqwest` con le chiamate a `self.api`
impl RemoteFS {
    fn add_path(&self, path: PathBuf) -> u64 {
        let mut paths = self.paths.lock().unwrap();
        for (ino, p) in paths.iter() {
            if *p == path {
                return *ino;
            }
        }
        let mut next_ino = self.next_ino.lock().unwrap();
        *next_ino += 1;
        paths.insert(*next_ino, path);
        *next_ino
    }
}

impl Filesystem for RemoteFS {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let parent_path = self.paths.lock().unwrap().get(&parent).unwrap().clone();
        let child_path = parent_path.join(name);
        
        match self.api.list_directory(&parent_path) {
            Ok(entries) => {
                if let Some(entry) = entries.iter().find(|e| e.name == name.to_str().unwrap()) {
                    let ino = self.add_path(child_path);
                    let attr = FileAttr {
                        ino,
                        size: entry.size,
                        kind: if entry.is_dir { FileType::Directory } else { FileType::RegularFile },
                        perm: if entry.is_dir { 0o755 } else { 0o644 },
                        nlink: 1, uid: _req.uid(), gid: _req.gid(),
                        // Inizializza gli altri campi...
                        blocks: 0, atime: UNIX_EPOCH, mtime: UNIX_EPOCH, ctime: UNIX_EPOCH, crtime: UNIX_EPOCH, rdev: 0, flags: 0, blksize: 512,
                    };
                    reply.entry(&TTL, &attr, 0);
                } else { reply.error(ENOENT); }
            }
            Err(_) => reply.error(ENOENT),
        }
    }
    // Implementa gli altri metodi (getattr, readdir, ecc.) in modo simile...
    fn readdir(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, mut reply: ReplyDirectory) {
        let path = self.paths.lock().unwrap().get(&ino).unwrap().clone();
        if offset == 0 {
            reply.add(ino, 0, FileType::Directory, ".");
            let parent_ino = if ino == FUSE_ROOT_INO { FUSE_ROOT_INO } else { self.add_path(path.parent().unwrap().to_path_buf()) };
            reply.add(parent_ino, 1, FileType::Directory, "..");

            if let Ok(entries) = self.api.list_directory(&path) {
                for (i, entry) in entries.iter().enumerate() {
                    let file_path = path.join(&entry.name);
                    let file_ino = self.add_path(file_path);
                    let file_type = if entry.is_dir { FileType::Directory } else { FileType::RegularFile };
                    if reply.add(file_ino, i as i64 + 2, file_type, &entry.name) { break; }
                }
            }
        }
        reply.ok();
    }

    fn read(&mut self, _req: &Request, ino: u64, _fh: u64, _offset: i64, _size: u32, _flags: i32, _lock_owner: Option<u64>, reply: ReplyData) {
        let path = self.paths.lock().unwrap().get(&ino).unwrap().clone();
        match self.api.read_file(&path) {
            Ok(data) => reply.data(&data),
            Err(_) => reply.error(ENOENT),
        }
    }

    // ... e così via per tutti gli altri metodi.
    // ... create, write, mkdir, unlink, rmdir
    // La logica è la stessa, basta sostituire le chiamate http con le chiamate a self.api
}