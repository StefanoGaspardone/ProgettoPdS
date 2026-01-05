use winfsp::filesystem::{
    DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, VolumeInfo,
    WideNameInfo, OpenFileInfo,
};
use winfsp::host::{FileSystemHost, VolumeParams};
use winfsp::{FspError, Result, U16CString, U16CStr}; 
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::ffi::c_void;
use std::cmp::min;

use crate::RemoteFilesystem;
use crate::FileInfo as RemoteFileInfo;

const WINFSP_ROOT_ID: u64 = 1;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const FILE_WRITE_DATA: u32 = 0x02;
const FILE_APPEND_DATA: u32 = 0x04;
const GENERIC_WRITE: u32 = 0x40000000;
const DELETE: u32 = 0x00010000;

fn unix_to_filetime(unix_secs: u64) -> u64 {
    (unix_secs as u64) * 10_000_000u64 + 116_444_736_000_000_000u64
}

fn current_filetime() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    unix_to_filetime(now)
}

fn create_file_info(ino: u64, file_data: &RemoteFileInfo) -> FileInfo {
    let timestamp = unix_to_filetime(file_data.timestamp);
    let is_dir = file_data.file_type == "dir";
    
    FileInfo {
        file_attributes: if is_dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL },
        creation_time: timestamp,
        last_access_time: timestamp,
        last_write_time: timestamp,
        change_time: timestamp,
        allocation_size: ((file_data.size as u64 + 4095) / 4096) * 4096,
        file_size: file_data.size as u64,
        hard_links: 1,
        reparse_tag: 0,
        index_number: ino,
        ea_size: 0,
    }
}

fn create_root_file_info() -> FileInfo {
    let now = current_filetime();
    FileInfo {
        file_attributes: FILE_ATTRIBUTE_DIRECTORY,
        creation_time: now,
        last_access_time: now,
        last_write_time: now,
        change_time: now,
        allocation_size: 0,
        file_size: 0,
        hard_links: 1,
        reparse_tag: 0,
        index_number: WINFSP_ROOT_ID,
        ea_size: 0,
    }
}

fn is_system_file(path: &str) -> bool {
    let name = path.split('/').last().unwrap_or("").to_lowercase();
    matches!(
        name.as_str(),
        "autorun.inf"
            | "desktop.ini"
            | "folder.jpg"
            | "folder.gif"
            | "front.jpg"
            | "thumbs.db"
            | "target.lnk"
            | "iconcache.db"
    ) || name.starts_with("albumart")
}

fn normalize_path(path: &str) -> String {
    let p = path.replace("\\", "/")
        .trim_matches('/')
        .to_string();
    if p.is_empty() { String::new() } else { p }
}

impl FileSystemContext for RemoteFilesystem {
    type FileContext = u64;

    fn get_file_info(
        &self,
        file_context: &Self::FileContext,
        file_info: &mut FileInfo,
    ) -> Result<()> {
        println!("Reading file info");
        if *file_context == WINFSP_ROOT_ID {
            *file_info = create_root_file_info();
            return Ok(());
        }

        let inode_cache = self.inode_cache.lock().unwrap();
        let path = match inode_cache.get(file_context) {
            Some(p) => p.clone(),
            None => return Err(FspError::NTSTATUS(-2147483646)), // STATUS_NO_SUCH_FILE
        };
        drop(inode_cache);

        let mut metadata_cache = self.metadata_cache.lock().unwrap();
        if let Some((_, file_data)) = metadata_cache.get(&path) {
            *file_info = create_file_info(*file_context, file_data);
            return Ok(());
        }
        drop(metadata_cache);

        let url = self.server_url.join(&format!("/stat/{}", path)).ok();
        if let Some(url) = url {
            let client = reqwest::Client::new();
            if let Ok(remote_info) = self.runtime.block_on(async {
                client.get(url).send().await?.json::<RemoteFileInfo>().await
            }) {
                *file_info = create_file_info(*file_context, &remote_info);
                let mut metadata_cache = self.metadata_cache.lock().unwrap();
                metadata_cache.insert(path, (*file_context, remote_info));
                return Ok(());
            }
        }
        println!("Reading error");
        Err(FspError::NTSTATUS(-2147483646))
    }

    fn get_security_by_name(
        &self,
        _file_name: &U16CStr,
        _security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> Result<FileSecurity> {
        println!("Reading get security by name");
        Ok(FileSecurity {
            reparse: false,
            sz_security_descriptor: 0,
            attributes: FILE_ATTRIBUTE_NORMAL,
        })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        _create_options: u32,
        _granted_access: u32,
        _open_file_info: &mut OpenFileInfo,
    ) -> Result<Self::FileContext> {
        println!("Opening file");
        let file_name_str = file_name.to_string().map_err(|_| FspError::NTSTATUS(-2147483646))?;
        let normalized_name = normalize_path(&file_name_str);
        
        if normalized_name.is_empty() {
            return Ok(WINFSP_ROOT_ID);
        }

        let metadata_cache = self.metadata_cache.lock().unwrap();
        if let Some((ino, _)) = metadata_cache.get(&normalized_name) {
            return Ok(*ino);
        }
        drop(metadata_cache);

        let url = self.server_url.join(&format!("/stat/{}", normalized_name)).ok();
        if let Some(url) = url {
            let client = reqwest::Client::new();
            if let Ok(file_info) = self.runtime.block_on(async {
                client.get(url).send().await?.json::<RemoteFileInfo>().await
            }) {
                let mut inode_cache = self.inode_cache.lock().unwrap();
                let mut metadata_cache = self.metadata_cache.lock().unwrap();
                let mut next_inode_guard = self.next_inode.lock().unwrap();
                
                let ino = *next_inode_guard;
                *next_inode_guard += 1;
                
                metadata_cache.insert(normalized_name.clone(), (ino, file_info));
                inode_cache.insert(ino, normalized_name);
                
                return Ok(ino);
            }
        }

        Err(FspError::NTSTATUS(-2147483646))
    }

    fn create(
        &self,
        file_name: &U16CStr,
        _create_options: u32,
        granted_access: u32,
        file_attributes: u32,
        _security_descriptor: Option<&[c_void]>,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        _is_dry_run: bool,
        _create_file_info: &mut OpenFileInfo,
    ) -> Result<Self::FileContext> {
        println!("Creating file");
        let file_name_str = file_name.to_string().map_err(|_| FspError::NTSTATUS(-1073741790))?;
        let normalized_name = normalize_path(&file_name_str);

        if normalized_name.is_empty() {
            return Err(FspError::NTSTATUS(-1073741790));
        }

        // Prevent "ghost" creations during read-only probes (like desktop.ini)
        let has_write_access = (granted_access & (GENERIC_WRITE | FILE_WRITE_DATA | FILE_APPEND_DATA)) != 0;
        let is_delete_intent = (granted_access & DELETE) != 0;
        let is_system_probe = is_system_file(&normalized_name);
        if !has_write_access || is_system_probe || is_delete_intent {
            return Err(FspError::NTSTATUS(-2147483646)); // STATUS_NO_SUCH_FILE
        }

        let is_dir = (file_attributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
        let client = reqwest::Client::new();

        let success = self.runtime.block_on(async {
            if is_dir {
                let url = self.server_url.join(&format!("/mkdir/{}", normalized_name)).ok()?;
                client.post(url).send().await.ok()?.status().is_success().then_some(())
            } else {
                let url = self.server_url.join(&format!("/files/{}", normalized_name)).ok()?;
                client.put(url)
                    .header("Content-Type", "text/plain")
                    .body("")
                    .send().await.ok()?.status().is_success().then_some(())
            }
        });

        if success.is_some() {
            let mut inode_cache = self.inode_cache.lock().unwrap();
            let mut metadata_cache = self.metadata_cache.lock().unwrap();
            let mut next_inode_guard = self.next_inode.lock().unwrap();

            let ino = *next_inode_guard;
            *next_inode_guard += 1;

            let new_info = RemoteFileInfo {
                name: normalized_name.split('/').last().unwrap_or("").to_string(),
                file_type: if is_dir { "dir".to_string() } else { "file".to_string() },
                size: 0,
                timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
                path: normalized_name.clone(),
                permissions: "755".to_string(),
            };

            metadata_cache.insert(normalized_name.clone(), (ino, new_info));
            inode_cache.insert(ino, normalized_name);
            return Ok(ino);
        }

        Err(FspError::NTSTATUS(-1073741790))
    }

    fn close(&self, _file_context: Self::FileContext) {
    }

    fn read(
        &self,
        file_context: &Self::FileContext,
        buffer: &mut [u8],
        offset: u64,
    ) -> Result<u32> {
        println!("Reading read");
        if *file_context == WINFSP_ROOT_ID {
            return Err(FspError::NTSTATUS(-1073741822));
        }

        let inode_cache = self.inode_cache.lock().unwrap();
        let path = inode_cache.get(file_context).cloned().ok_or(FspError::NTSTATUS(-2147483646))?;
        drop(inode_cache);

        let url = self.server_url.join(&format!("/files/{}", path)).ok();
        if let Some(url) = url {
            let client = reqwest::Client::new();
            let data = self.runtime.block_on(async {
                match client.get(url).send().await {
                    Ok(res) => res.bytes().await.ok(),
                    Err(_) => None,
                }
            });

            if let Some(bytes) = data {
                let file_len = bytes.len() as u64;
                if offset >= file_len {
                    return Ok(0);
                }
                
                let available = file_len - offset;
                let bytes_to_read = min(buffer.len(), available as usize);
                
                buffer[..bytes_to_read].copy_from_slice(&bytes[offset as usize..offset as usize + bytes_to_read]);
                return Ok(bytes_to_read as u32);
            }
        }

        Err(FspError::NTSTATUS(-1073741789))
    }

    fn write(
        &self,
        file_context: &Self::FileContext,
        buffer: &[u8],
        offset: u64,
        _write_to_end_of_file: bool,
        _constrained_io: bool,
        _file_info: &mut FileInfo,
    ) -> Result<u32> {
        println!("Reading write");
        if *file_context == WINFSP_ROOT_ID {
            return Err(FspError::NTSTATUS(-1073741790));
        }
        
        if offset > 0 {
            eprintln!("Random access write not supported");
            return Err(FspError::NTSTATUS(-1073741822));
        }

        let inode_cache = self.inode_cache.lock().unwrap();
        let path = inode_cache.get(file_context).cloned().ok_or(FspError::NTSTATUS(-2147483646))?;
        drop(inode_cache);

        let url = self.server_url.join(&format!("/files/{}", path)).ok();
        if let Some(url) = url {
            let client = reqwest::Client::new();
            let body = buffer.to_vec();
            let success = self.runtime.block_on(async {
                client.put(url)
                    .header("Content-Type", "application/octet-stream")
                    .body(body)
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false)
            });

            if success {
                let mut metadata_cache = self.metadata_cache.lock().unwrap();
                if let Some((_, info)) = metadata_cache.get_mut(&path) {
                    info.size = buffer.len();
                }
                return Ok(buffer.len() as u32);
            }
        }

        Err(FspError::NTSTATUS(-1073741789))
    }

    fn read_directory(
        &self,
        file_context: &Self::FileContext,
        _pattern: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> Result<u32> {
        let mut cursor = 0u32;
        println!("Reading directory");
        let path = if *file_context == WINFSP_ROOT_ID {
            String::new()
        } else {
            let inode_cache = self.inode_cache.lock().unwrap();
            match inode_cache.get(file_context) {
                Some(p) => p.clone(),
                None => return Err(FspError::NTSTATUS(-2147483646)),
            }
        };

        let url = self.server_url.join(&format!("/list/{}", path)).ok();
        if url.is_none() { return Err(FspError::NTSTATUS(-2147483646)); }

        let client = reqwest::Client::new();
        let mut files: Vec<RemoteFileInfo> = match self.runtime.block_on(async {
            client.get(url.unwrap()).send().await?.json().await
        }) {
            Ok(f) => f,
            Err(_) => return Ok(0),
        };

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        
        let dot = RemoteFileInfo { 
            name: ".".to_string(), 
            file_type: "dir".to_string(), 
            size: 0, 
            timestamp: now,
            path: ".".to_string(), 
            permissions: "755".to_string() 
        };
        let dotdot = RemoteFileInfo { 
            name: "..".to_string(), 
            file_type: "dir".to_string(), 
            size: 0, 
            timestamp: now,
            path: "..".to_string(), 
            permissions: "755".to_string() 
        };
        
        files.push(dot);
        files.push(dotdot);

        files.sort_by(|a, b| a.name.cmp(&b.name));

        let mut metadata_cache = self.metadata_cache.lock().unwrap();
        let mut inode_cache = self.inode_cache.lock().unwrap();
        let mut next_inode_guard = self.next_inode.lock().unwrap();

        for file_info in files {
            let name_wide = match U16CString::from_str(&file_info.name) {
                Ok(s) => s,
                Err(_) => continue,
            };

            if let Some(marker_str) = marker.inner_as_cstr() {
                if name_wide.as_ucstr() <= marker_str {
                    continue;
                }
            }

            let ino;
            if file_info.name == "." {
                ino = *file_context;
            } else if file_info.name == ".." {
                ino = if *file_context == WINFSP_ROOT_ID { WINFSP_ROOT_ID } else { WINFSP_ROOT_ID };
            } else {
                let file_path = if path.is_empty() {
                    file_info.name.clone()
                } else {
                    format!("{}/{}", path, file_info.name)
                };

                if let Some((existing_ino, _)) = metadata_cache.get(&file_path) {
                    ino = *existing_ino;
                } else {
                    ino = *next_inode_guard;
                    *next_inode_guard += 1;
                    metadata_cache.insert(file_path.clone(), (ino, file_info.clone()));
                    inode_cache.insert(ino, file_path);
                }
            }

            let mut dir_info = DirInfo::<255>::new();
            *dir_info.file_info_mut() = create_file_info(ino, &file_info);
            
            if let Err(_) = dir_info.set_name(&file_info.name) {
                continue;
            }

            if !dir_info.append_to_buffer(buffer, &mut cursor) {
                break;
            }
        }

        Ok(cursor)
    }

    fn get_volume_info(&self, volume_info: &mut VolumeInfo) -> Result<()> {
        println!("Reading volume");
        volume_info.total_size = 1024 * 1024 * 1024 * 100;
        volume_info.free_size = 1024 * 1024 * 1024 * 50;
        volume_info.set_volume_label("RemoteFS");
        Ok(())
    }

    fn set_file_size(
        &self,
        file_context: &Self::FileContext,
        new_size: u64,
        _set_allocation_size: bool,
        file_info: &mut FileInfo,
    ) -> Result<()> {
        println!("Reading setfile size");
        let mut metadata_cache = self.metadata_cache.lock().unwrap();
        let inode_cache = self.inode_cache.lock().unwrap();
        
        let path = if *file_context == WINFSP_ROOT_ID {
             return Err(FspError::NTSTATUS(-1073741790));
        } else {
             inode_cache.get(file_context).cloned().ok_or(FspError::NTSTATUS(-2147483646))?
        };
        drop(inode_cache);

        if let Some((_, info)) = metadata_cache.get_mut(&path) {
            info.size = new_size as usize;
            *file_info = create_file_info(*file_context, info);
            return Ok(());
        }

        Err(FspError::NTSTATUS(-2147483646))
    }
}

pub fn run_winfsp_client(filesystem: RemoteFilesystem) {
    println!("Starting WinFSP filesystem client");

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        println!("Received shutdown signal, unmounting...");
        r.store(false, Ordering::SeqCst);
    }).expect("Error setting Ctrl-C handler");

    match FileSystemHost::new(VolumeParams::default(), filesystem) {
        Ok(mut host) => {
            if let Err(e) = host.start() {
                eprintln!("Failed to start filesystem dispatcher: {:?}", e);
                return;
            }

            match host.mount("M:") {
                Ok(_) => {
                    println!("Filesystem successfully mounted at M:");
                    while running.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(100));
                    }
                    host.unmount();
                }
                Err(err) => eprintln!("Failed to mount at M: {:?}", err),
            }
        }
        Err(err) => eprintln!("Failed to create host: {:?}", err),
    }
}