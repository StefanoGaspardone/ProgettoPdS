use winfsp::filesystem::{FileSystemContext, FileInfo, VolumeInfo, DirMarker, FileSecurity, DirInfo, WideNameInfo};
use winfsp::host::{FileSystemHost, VolumeParams};
use winfsp::{U16CStr, FspError, Result};
use std::time::{SystemTime, UNIX_EPOCH};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::ffi::c_void;

use crate::RemoteFilesystem;
use crate::FileInfo as RemoteFileInfo;

const WINFSP_ROOT_ID: u64 = 1;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

// Convert Unix timestamp to Windows FILETIME (100-nanosecond intervals since 1601-01-01)
fn unix_to_filetime(unix_secs: u64) -> u64 {
    (unix_secs as u64) * 10_000_000u64 + 116_444_736_000_000_000u64
}

// Get current time as Windows FILETIME
fn current_filetime() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    unix_to_filetime(now)
}

// Create FileInfo struct for WinFSP
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

// Create root directory FileInfo
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

fn normalize_path(path: &str) -> String {
    path.replace("\\", "/")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string()
}

impl FileSystemContext for RemoteFilesystem {
    type FileContext = u64;

    fn get_file_info(
        &self,
        file_context: &Self::FileContext,
        file_info: &mut FileInfo,
    ) -> Result<()> {
        if *file_context == WINFSP_ROOT_ID {
            *file_info = create_root_file_info();
            return Ok(());
        }

        let inode_cache = self.inode_cache.lock().unwrap();
        let path = match inode_cache.get(file_context) {
            Some(p) => p.clone(),
            None => {
                eprintln!("get_file_info: inode {} not found in cache", file_context);
                return Err(FspError::NTSTATUS(-2147483646)); // STATUS_NO_SUCH_FILE
            }
        };
        drop(inode_cache);

        let metadata_cache = self.metadata_cache.lock().unwrap();
        
        if let Some((_, file_data)) = metadata_cache.get(&path) {
            *file_info = create_file_info(*file_context, file_data);
            return Ok(());
        }
        drop(metadata_cache);

        // Try to fetch from server if not in cache
        let url = self.server_url.join(&format!("/stat/{}", path)).ok();
        if let Some(url) = url {
            if let Ok(remote_info) = self.runtime.block_on(async {
                let client = reqwest::Client::new();
                let res = client.get(url).send().await?;
                res.json::<RemoteFileInfo>().await
            }) {
                *file_info = create_file_info(*file_context, &remote_info);
                let mut metadata_cache = self.metadata_cache.lock().unwrap();
                metadata_cache.insert(path, (*file_context, remote_info));
                return Ok(());
            }
        }

        eprintln!("get_file_info: failed to get info for path: {}", path);
        Err(FspError::NTSTATUS(-2147483646)) // STATUS_NO_SUCH_FILE
    }

    fn get_security_by_name(
        &self,
        _file_name: &U16CStr,
        _security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> Result<FileSecurity> {
        // Return default security for all files
        Ok(FileSecurity {
            reparse: false,
            sz_security_descriptor: 0,
            attributes: 0x80, // FILE_ATTRIBUTE_NORMAL
        })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        _create_options: u32,
        _granted_access: u32,
        _open_file_info: &mut winfsp::filesystem::OpenFileInfo,
    ) -> Result<Self::FileContext> {
        let file_name_str = file_name.to_string().map_err(|_| FspError::NTSTATUS(-2147483646))?;
        let normalized_name = normalize_path(&file_name_str);
        
        // Handle root directory
        if normalized_name.is_empty() || normalized_name == "/" {
            return Ok(WINFSP_ROOT_ID);
        }

        let inode_cache = self.inode_cache.lock().unwrap();
        let metadata_cache = self.metadata_cache.lock().unwrap();

        // Check cache first
        if let Some((ino, _)) = metadata_cache.get(&normalized_name) {
            return Ok(*ino);
        }
        drop(inode_cache);
        drop(metadata_cache);

        // Try to fetch from server
        let url = self.server_url.join(&format!("/stat/{}", normalized_name)).ok();
        if let Some(url) = url {
            if let Ok(file_info) = self.runtime.block_on(async {
                let client = reqwest::Client::new();
                let res = client.get(url).send().await?;
                res.json::<RemoteFileInfo>().await
            }) {
                let mut inode_cache = self.inode_cache.lock().unwrap();
                let mut metadata_cache = self.metadata_cache.lock().unwrap();
                
                let mut next_inode_guard = self.next_inode.lock().unwrap();
                let ino = *next_inode_guard;
                *next_inode_guard += 1;
                drop(next_inode_guard);
                metadata_cache.insert(normalized_name.clone(), (ino, file_info));
                inode_cache.insert(ino, normalized_name);
                return Ok(ino);
            }
        }

        Err(FspError::NTSTATUS(-2147483646))
    }

    fn close(&self, _file_context: Self::FileContext) {
        // Nothing to do for closing
    }

    fn read(
        &self,
        file_context: &Self::FileContext,
        _buffer: &mut [u8],
        _offset: u64,
    ) -> Result<u32> {
        let inode_cache = self.inode_cache.lock().unwrap();
        let path = if let Some(p) = inode_cache.get(file_context) {
            p.clone()
        } else {
            return Err(FspError::NTSTATUS(-2147483646));
        };
        drop(inode_cache);

        let url = self.server_url.join(&format!("/files/{}", path)).ok();
        if let Some(url) = url {
            let _data = self.runtime.block_on(async {
                let client = reqwest::Client::new();
                match client.get(url).send().await {
                    Ok(res) => res.bytes().await.ok(),
                    Err(_) => None,
                }
            });
            return Ok(0);
        }

        Err(FspError::NTSTATUS(-1073741789))
    }

    fn write(
        &self,
        file_context: &Self::FileContext,
        buffer: &[u8],
        _offset: u64,
        _write_to_end_of_file: bool,
        _constrained_io: bool,
        _file_info: &mut FileInfo,
    ) -> Result<u32> {
        let inode_cache = self.inode_cache.lock().unwrap();
        let path = if let Some(p) = inode_cache.get(file_context) {
            p.clone()
        } else {
            return Err(FspError::NTSTATUS(-2147483646));
        };
        drop(inode_cache);

        let url = self.server_url.join(&format!("/files/{}", path)).ok();
        if let Some(url) = url {
            let success = self.runtime.block_on(async {
                let client = reqwest::Client::new();
                match client.put(url).header("Content-Type", "application/octet-stream").body(buffer.to_vec()).send().await {
                    Ok(res) => res.status().is_success(),
                    Err(_) => false,
                }
            });

            if success {
                return Ok(buffer.len() as u32);
            }
        }

        Err(FspError::NTSTATUS(-1073741789))
    }

    fn read_directory(
        &self,
        file_context: &Self::FileContext,
        _pattern: Option<&U16CStr>,
        _marker: DirMarker,
        buffer: &mut [u8],
    ) -> Result<u32> {
        // Get the path for this directory
        let inode_cache = self.inode_cache.lock().unwrap();
        let path = if let Some(p) = inode_cache.get(file_context) {
            p.clone()
        } else {
            return Err(FspError::NTSTATUS(-2147483646));
        };
        drop(inode_cache);

        // Fetch directory listing from server
        let url = self.server_url.join(&format!("/list/{}", path)).ok();
        if url.is_none() {
            return Err(FspError::NTSTATUS(-2147483646));
        }

        let files: Vec<RemoteFileInfo> = match self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client.get(url.unwrap()).send().await?;
            res.json().await
        }) {
            Ok(f) => f,
            Err(_) => {
                return Ok(0);
            }
        };

        // Update cache with directory entries
        let mut metadata_cache = self.metadata_cache.lock().unwrap();
        let mut inode_cache = self.inode_cache.lock().unwrap();
        let mut next_inode_guard = self.next_inode.lock().unwrap();
        
        let mut cursor = 0u32;
        
        for file_info in &files {
            let file_path = if path.is_empty() {
                file_info.name.clone()
            } else {
                format!("{}/{}", path, file_info.name)
            };
            
            // Ensure each file has an inode
            let ino = if let Some((existing_ino, _)) = metadata_cache.get(&file_path) {
                *existing_ino
            } else {
                let new_ino = *next_inode_guard;
                *next_inode_guard += 1;
                metadata_cache.insert(file_path.clone(), (new_ino, file_info.clone()));
                inode_cache.insert(new_ino, file_path);
                new_ino
            };
            
            // Create DirInfo entry for this file
            let mut dir_info: DirInfo<255> = DirInfo::new();
            *dir_info.file_info_mut() = create_file_info(ino, file_info);
            
            // Set the filename using OsStr trait
            use std::ffi::OsStr;
            use std::os::windows::ffi::OsStrExt;
            let wide: Vec<u16> = OsStr::new(&file_info.name)
                .encode_wide()
                .chain(Some(0))
                .collect();
            let _ = dir_info.set_name_raw(&wide[..]);
            
            // Append to buffer - this serializes the entry
            if !dir_info.append_to_buffer(buffer, &mut cursor) {
                // Buffer full, stop adding more entries
                break;
            }
        }
        
        // Finalize the buffer to mark end of directory
        let _ = DirInfo::<255>::finalize_buffer(buffer, &mut cursor);
        
        Ok(cursor)
    }

    fn get_volume_info(&self, volume_info: &mut VolumeInfo) -> Result<()> {
        volume_info.total_size = 1u64 << 40;
        volume_info.free_size = 1u64 << 40;
        Ok(())
    }

    fn set_file_size(
        &self,
        file_context: &Self::FileContext,
        new_size: u64,
        _set_allocation_size: bool,
        file_info: &mut FileInfo,
    ) -> Result<()> {
        let mut metadata_cache = self.metadata_cache.lock().unwrap();
        let inode_cache = self.inode_cache.lock().unwrap();
        
        let path = if let Some(p) = inode_cache.get(file_context) {
            p.clone()
        } else {
            return Err(FspError::NTSTATUS(-2147483646));
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

    // Create a FileSystemHost
    match FileSystemHost::new(VolumeParams::default(), filesystem) {
        Ok(mut host) => {
            // Start the filesystem dispatcher
            if let Err(e) = host.start() {
                eprintln!("Failed to start filesystem dispatcher: {:?}", e);
                return;
            }

            // Mount the filesystem at M: drive
            match host.mount("M:") {
                Ok(_) => {
                    println!("Filesystem successfully mounted at M:");

                    // Keep the service running while not interrupted
                    while running.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(100));
                    }

                    println!("Stopping filesystem service...");
                    host.unmount();
                    println!("Filesystem unmounted, exiting.");
                }
                Err(err) => {
                    eprintln!("Failed to mount filesystem at M:: {:?}", err);
                }
            }
        }
        Err(err) => {
            eprintln!("Failed to create filesystem host: {:?}", err);
        }
    }
}