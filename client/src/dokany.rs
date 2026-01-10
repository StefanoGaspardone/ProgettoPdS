use dokan::{
    CreateFileInfo, FileSystemHandler, FileSystemMounter, FileTimeOperation, FillDataResult, FindData, MountOptions, OperationInfo, VolumeInfo
};
use dokan_sys::DOKAN_IO_SECURITY_CONTEXT;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use widestring::{U16CStr, U16CString}; // UCString is not needed, using U16CStr
use crate::{RemoteFilesystem, FileInfo as RemoteFileInfo};

// Constants for File Attributes (Win32)
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x00000010;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x00000080;
const FILE_CASE_PRESERVED_NAMES: u32 = 0x00000002;
const FILE_UNICODE_ON_DISK: u32 = 0x00000004;

// Generic error code (NTSTATUS -1 is generic error)
const DOKAN_ERROR: i32 = -1073741823; // STATUS_UNSUCCESSFUL
const STATUS_OBJECT_NAME_NOT_FOUND: i32 = -1073741772;
const STATUS_OBJECT_NAME_COLLISION: i32 = -1073741771;
const STATUS_NOT_A_DIRECTORY: i32 = -1073741565;
const STATUS_FILE_IS_A_DIRECTORY: i32 = -1073741638;
const FILE_NON_DIRECTORY_FILE: u32 = 0x00000040;

const FILE_DIRECTORY_FILE: u32 = 0x00000001;
const FILE_SUPERSEDE: u32 = 0;
const FILE_OPEN: u32 = 1;
const FILE_CREATE: u32 = 2;
const FILE_OPEN_IF: u32 = 3;
const FILE_OVERWRITE: u32 = 4;
const FILE_OVERWRITE_IF: u32 = 5;

impl<'c, 'h> FileSystemHandler<'c, 'h> for RemoteFilesystem 
where 'h: 'c
{
    type Context = ();

    fn create_file(
        &'h self,
        file_name: &U16CStr,
        _security_context: &DOKAN_IO_SECURITY_CONTEXT,
        _access_mode: u32,
        _file_attributes: u32,
        _share_access: u32,
        create_disposition: u32,
        create_options: u32,
        _info: &mut OperationInfo<'c, 'h, Self>
    ) -> Result<CreateFileInfo<Self::Context>, i32> {
        let path_str = file_name.to_string_lossy();
        let path = normalize_path(&path_str);

        if is_system_file(&path) {
            return Err(STATUS_OBJECT_NAME_NOT_FOUND);
        }

        let is_dir_request = (create_options & FILE_DIRECTORY_FILE) != 0;
        let non_dir_request = (create_options & FILE_NON_DIRECTORY_FILE) != 0;

        if path.is_empty() {
            if is_dir_request || !non_dir_request {
                return Ok(CreateFileInfo {
                    context: (),
                    is_dir: true,
                    new_file_created: false,
                });
            } else {
                return Err(STATUS_FILE_IS_A_DIRECTORY);
            }
        }

        let mut exists = false;
        let mut is_dir = false;

        {
            let metadata_cache = self.metadata_cache.lock().unwrap();
            if let Some((_, info)) = metadata_cache.get(&path) {
                exists = true;
                is_dir = info.file_type == "dir";
            }
        }

        if !exists {
            let url = self.server_url.join(&format!("/stat/{}", path)).map_err(|_| DOKAN_ERROR)?;
            let res = self.runtime.block_on(async {
                let client = reqwest::Client::new();
                client.get(url).send().await
            });

            if let Ok(response) = res {
                if response.status().is_success() {
                    if let Ok(info) = self.runtime.block_on(response.json::<RemoteFileInfo>()) {
                        exists = true;
                        is_dir = info.file_type == "dir";
                        
                        let mut metadata_cache = self.metadata_cache.lock().unwrap();
                        let mut inode_cache = self.inode_cache.lock().unwrap();
                        if !metadata_cache.contains_key(&path) {
                            let mut next_inode = self.next_inode.lock().unwrap();
                            let ino = *next_inode;
                            *next_inode += 1;
                            metadata_cache.insert(path.clone(), (ino, info));
                            inode_cache.insert(ino, path.clone());
                        }
                    }
                }
            }
        }

        if exists {
            if is_dir && non_dir_request {
                return Err(STATUS_FILE_IS_A_DIRECTORY);
            }
            if !is_dir && is_dir_request {
                return Err(STATUS_NOT_A_DIRECTORY);
            }
        }

        match create_disposition {
            FILE_OPEN => {
                if !exists {
                    return Err(STATUS_OBJECT_NAME_NOT_FOUND);
                }
                return Ok(CreateFileInfo {
                    context: (),
                    is_dir,
                    new_file_created: false,
                });
            },
            FILE_CREATE => {
                if exists {
                    return Err(STATUS_OBJECT_NAME_COLLISION);
                }
            },
            FILE_OPEN_IF => {
                if exists {
                    return Ok(CreateFileInfo {
                        context: (),
                        is_dir,
                        new_file_created: false,
                    });
                }
            },
            FILE_OVERWRITE => {
                if !exists {
                    return Err(STATUS_OBJECT_NAME_NOT_FOUND);
                }
                if is_dir {
                    return Err(DOKAN_ERROR);
                }
            },
            FILE_OVERWRITE_IF => {
                if exists && is_dir {
                    return Err(DOKAN_ERROR);
                }
            },
            FILE_SUPERSEDE => {
                if exists && is_dir {
                    return Err(DOKAN_ERROR);
                }
            },
            _ => return Err(DOKAN_ERROR),
        }

        // Windows redirection semantics:
        // - `echo ... > file` typically opens with overwrite/truncate semantics.
        // - `echo ... >> file` typically opens with append, and writes at end.
        // Truncation is best handled here and in `set_end_of_file`.
        let should_truncate_existing_file = matches!(
            create_disposition,
            FILE_OVERWRITE | FILE_OVERWRITE_IF | FILE_SUPERSEDE
        ) && exists && !is_dir && !is_dir_request;

        if should_truncate_existing_file {
            let url = self
                .server_url
                .join(&format!("/truncate/{}", path))
                .map_err(|_| DOKAN_ERROR)?;
            let truncated = self.runtime.block_on(async {
                let client = reqwest::Client::new();
                let res = client
                    .post(url)
                    .json(&serde_json::json!({"size": 0}))
                    .send()
                    .await;
                matches!(res, Ok(r) if r.status().is_success())
            });

            if truncated {
                let stat_url = self
                    .server_url
                    .join(&format!("/stat/{}", path))
                    .map_err(|_| DOKAN_ERROR)?;
                let updated = self.runtime.block_on(async {
                    let client = reqwest::Client::new();
                    let res = client.get(stat_url).send().await;
                    match res {
                        Ok(r) if r.status().is_success() => r.json::<RemoteFileInfo>().await.ok(),
                        _ => None,
                    }
                });

                if let Some(info) = updated {
                    let mut metadata_cache = self.metadata_cache.lock().unwrap();
                    if let Some((ino, _)) = metadata_cache.get(&path).cloned() {
                        metadata_cache.insert(path.clone(), (ino, info));
                    }
                }
            }
        }

        if is_dir_request {
            let url = self.server_url.join(&format!("/mkdir/{}", path)).map_err(|_| DOKAN_ERROR)?;
            let success = self.runtime.block_on(async {
                let client = reqwest::Client::new();
                let res = client.post(url).send().await;
                matches!(res, Ok(r) if r.status().is_success())
            });

            if success {
                let mut metadata_cache = self.metadata_cache.lock().unwrap();
                let mut inode_cache = self.inode_cache.lock().unwrap();
                let mut next_inode = self.next_inode.lock().unwrap();
                let ino = *next_inode;
                *next_inode += 1;

                let info = RemoteFileInfo {
                    name: path.split('/').last().unwrap_or("").to_string(),
                    path: path.clone(),
                    file_type: "dir".to_string(),
                    size: 0,
                    timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
                    permissions: "755".to_string(),
                };

                metadata_cache.insert(path.clone(), (ino, info));
                inode_cache.insert(ino, path.clone());

                return Ok(CreateFileInfo {
                    context: (),
                    is_dir: true,
                    new_file_created: true,
                });
            }
        } else {
            let url = self.server_url.join(&format!("/files/{}", path)).map_err(|_| DOKAN_ERROR)?;
            let success = self.runtime.block_on(async {
                let client = reqwest::Client::new();
                let res = client.put(url)
                    .header("Content-Type", "application/octet-stream")
                    .body("")
                    .send().await;
                matches!(res, Ok(r) if r.status().is_success())
            });

            if success {
                let mut metadata_cache = self.metadata_cache.lock().unwrap();
                let mut inode_cache = self.inode_cache.lock().unwrap();
                
                let ino = if let Some((i, _)) = metadata_cache.get(&path) {
                    *i
                } else {
                    let mut next_inode = self.next_inode.lock().unwrap();
                    let i = *next_inode;
                    *next_inode += 1;
                    inode_cache.insert(i, path.clone());
                    i
                };

                let info = RemoteFileInfo {
                    name: path.split('/').last().unwrap_or("").to_string(),
                    path: path.clone(),
                    file_type: "file".to_string(),
                    size: 0,
                    timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
                    permissions: "644".to_string(),
                };

                metadata_cache.insert(path.clone(), (ino, info));

                return Ok(CreateFileInfo {
                    context: (),
                    is_dir: false,
                    new_file_created: true,
                });
            }
        }

        Err(DOKAN_ERROR)
    }

    fn read_file(
        &'h self,
        file_name: &U16CStr,
        offset: i64,
        buffer: &mut [u8],
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) -> Result<u32, i32> {
        let path_str = file_name.to_string_lossy();
        let path = normalize_path(&path_str);

        println!("Reading file: {}", path);

        let url = self.server_url.join(&format!("/files/{}", path)).map_err(|_| DOKAN_ERROR)?;
        
        let data = self.runtime.block_on(async {
            let client = reqwest::Client::new();
            match client.get(url).send().await {
                Ok(res) if res.status().is_success() => res.bytes().await.ok(),
                _ => None,
            }
        });

        if let Some(content) = data {
            let content_len = content.len() as i64;
            if offset >= content_len {
                return Ok(0);
            }

            let available = content_len - offset;
            let to_read = std::cmp::min(available as usize, buffer.len());
            
            buffer[..to_read].copy_from_slice(&content[offset as usize..offset as usize + to_read]);
            Ok(to_read as u32)
        } else {
            Err(DOKAN_ERROR)
        }
    }

    fn write_file(
        &'h self,
        file_name: &U16CStr,
        offset: i64,
        buffer: &[u8],
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) -> Result<u32, i32> {
        let path_str = file_name.to_string_lossy();
        let path = normalize_path(&path_str);

        println!("Writing file: {}", path);
        
        // Dokan provides an explicit offset. For append-style handles, Dokan may pass -1.
        let effective_offset = if offset >= 0 {
            offset
        } else {
            // Fallback: compute end-of-file for append.
            let cached_size = {
                let metadata_cache = self.metadata_cache.lock().unwrap();
                metadata_cache.get(&path).map(|(_, info)| info.size as i64)
            };

            if let Some(size) = cached_size {
                size
            } else {
                let stat_url = self
                    .server_url
                    .join(&format!("/stat/{}", path))
                    .map_err(|_| DOKAN_ERROR)?;
                let info = self.runtime.block_on(async {
                    let client = reqwest::Client::new();
                    let res = client.get(stat_url).send().await;
                    match res {
                        Ok(r) if r.status().is_success() => r.json::<RemoteFileInfo>().await.ok(),
                        _ => None,
                    }
                });
                info.map(|i| i.size as i64).unwrap_or(0)
            }
        };

        let url = self
            .server_url
            .join(&format!("/files/{}?offset={}", path, effective_offset))
            .map_err(|_| DOKAN_ERROR)?;

        let status = self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client
                .put(url)
                .header("Content-Type", "application/octet-stream")
                .body(buffer.to_vec())
                .send()
                .await;
            res.ok().map(|r| r.status())
        });

        match status {
            Some(s) if s.is_success() => {
                // Refresh metadata cache for size/timestamp.
                let stat_url = self
                    .server_url
                    .join(&format!("/stat/{}", path))
                    .map_err(|_| DOKAN_ERROR)?;
                let updated = self.runtime.block_on(async {
                    let client = reqwest::Client::new();
                    let res = client.get(stat_url).send().await;
                    match res {
                        Ok(r) if r.status().is_success() => r.json::<RemoteFileInfo>().await.ok(),
                        _ => None,
                    }
                });

                if let Some(info) = updated {
                    let mut metadata_cache = self.metadata_cache.lock().unwrap();
                    if let Some((ino, _)) = metadata_cache.get(&path).cloned() {
                        metadata_cache.insert(path.clone(), (ino, info));
                    }
                }

                Ok(buffer.len() as u32)
            }
            _ => Err(DOKAN_ERROR),
        }
    }

    fn get_file_information(
        &'h self,
        file_name: &U16CStr,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) -> Result<dokan::FileInfo, i32> {
        let path_str = file_name.to_string_lossy();
        let path = normalize_path(&path_str);
        println!("Getting file info: {}", path);
        if path.is_empty() || path == "/" || path == "\\" {
             return Ok(dokan::FileInfo {
                attributes: FILE_ATTRIBUTE_DIRECTORY,
                creation_time: UNIX_EPOCH,
                last_access_time: UNIX_EPOCH,
                last_write_time: UNIX_EPOCH,
                file_size: 0,
                number_of_links: 1,
                file_index: 1,
            });
        }

        let mut metadata_cache = self.metadata_cache.lock().unwrap();
        
        if let Some((ino, info)) = metadata_cache.get(&path) {
             let sys_time = UNIX_EPOCH + Duration::from_secs(info.timestamp);
             return Ok(dokan::FileInfo {
                attributes: if info.file_type == "dir" { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL },
                creation_time: sys_time,
                last_access_time: sys_time,
                last_write_time: sys_time,
                file_size: info.size as u64,
                number_of_links: 1,
                file_index: *ino,
            });
        }

        let url = self.server_url.join(&format!("/stat/{}", path)).map_err(|_| DOKAN_ERROR)?;
        let file_info_res = self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client.get(url).send().await;
             match res {
                Ok(r) if r.status().is_success() => r.json::<RemoteFileInfo>().await.ok(),
                _ => None,
            }
        });

        if let Some(info) = file_info_res {
             let ino = if let Some((existing_ino, _)) = metadata_cache.get(&path) {
                 *existing_ino
             } else {
                 let mut next_inode = self.next_inode.lock().unwrap();
                 let val = *next_inode;
                 *next_inode += 1;
                 val
             };
             metadata_cache.insert(path.clone(), (ino, info.clone()));

             let sys_time = UNIX_EPOCH + Duration::from_secs(info.timestamp);
             Ok(dokan::FileInfo {
                attributes: if info.file_type == "dir" { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL },
                creation_time: sys_time,
                last_access_time: sys_time,
                last_write_time: sys_time,
                file_size: info.size as u64,
                number_of_links: 1,
                file_index: ino,
            })
        } else {
            Err(DOKAN_ERROR)
        }
    }

    fn find_files(
        &'h self,
        file_name: &U16CStr,
        mut fill_find_data: impl FnMut(&FindData) -> FillDataResult,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) -> Result<(), i32> {
        let path_str = file_name.to_string_lossy();
        let path = normalize_path(&path_str);

        println!("Listing directory: {}", path);

        let url = self.server_url.join(&format!("/list/{}", path)).map_err(|_| DOKAN_ERROR)?;
        
        let files = self.runtime.block_on(async {
            let client = reqwest::Client::new();
            match client.get(url).send().await {
                Ok(res) if res.status().is_success() => res.json::<Vec<RemoteFileInfo>>().await.ok(),
                _ => None,
            }
        });

        if let Some(files) = files {
            let mut metadata_cache = self.metadata_cache.lock().unwrap();
            
            for file in files {
                let ino = if let Some((existing_ino, _)) = metadata_cache.get(&file.path) {
                    *existing_ino
                } else {
                    let mut next_inode = self.next_inode.lock().unwrap();
                    let val = *next_inode;
                    *next_inode += 1;
                    val
                };

                metadata_cache.insert(file.path.clone(), (ino, file.clone()));
                let sys_time = UNIX_EPOCH + Duration::from_secs(file.timestamp);
                
                let data = FindData {
                    attributes: if file.file_type == "dir" { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL },
                    creation_time: sys_time,
                    last_access_time: sys_time,
                    last_write_time: sys_time,
                    file_size: file.size as u64,
                    file_name: U16CString::from_str(&file.name).map_err(|_| DOKAN_ERROR)?,
                };
                let _ = fill_find_data(&data);
            }
            Ok(())
        } else {
            Err(DOKAN_ERROR)
        }
    }

    fn delete_file(
        &'h self,
        _file_name: &U16CStr,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) -> Result<(), i32> {
        // This is a permission check. Return Ok to allow the OS to set the delete flag.
        Ok(())
    }

    fn delete_directory(
        &'h self,
        file_name: &U16CStr,
        info: &OperationInfo<'c, 'h, Self>,
        context: &'c Self::Context
    ) -> Result<(), i32> {
        self.delete_file(file_name, info, context)
    }

    fn cleanup(
        &'h self,
        file_name: &U16CStr,
        info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) {
        if info.delete_on_close() {
            let path = normalize_path(&file_name.to_string_lossy());
            println!("Cleaning up (deleting): {}", path);

            let url = self.server_url.join(&format!("/files/{}", path)).ok();
            if let Some(url) = url {
                let _ = self.runtime.block_on(async {
                    let client = reqwest::Client::new();
                    let res = client.delete(url).send().await;
                    if let Ok(r) = res {
                        if r.status().is_success() {
                            let mut metadata_cache = self.metadata_cache.lock().unwrap();
                            metadata_cache.remove(&path);
                        }
                    }
                });
            }
        }
    }

    fn move_file(
        &'h self,
        _file_name: &U16CStr,
        _new_file_name: &U16CStr,
        _replace_if_existing: bool,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) -> Result<(), i32> {
        Err(DOKAN_ERROR)
    }

    fn set_end_of_file(
        &'h self,
        file_name: &U16CStr,
        offset: i64,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) -> Result<(), i32> {
        if offset < 0 {
            return Ok(());
        }

        let path_str = file_name.to_string_lossy();
        let path = normalize_path(&path_str);

        let url = self
            .server_url
            .join(&format!("/truncate/{}", path))
            .map_err(|_| DOKAN_ERROR)?;
        let truncated = self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client
                .post(url)
                .json(&serde_json::json!({"size": offset}))
                .send()
                .await;
            matches!(res, Ok(r) if r.status().is_success())
        });

        if truncated {
            let stat_url = self
                .server_url
                .join(&format!("/stat/{}", path))
                .map_err(|_| DOKAN_ERROR)?;
            let updated = self.runtime.block_on(async {
                let client = reqwest::Client::new();
                let res = client.get(stat_url).send().await;
                match res {
                    Ok(r) if r.status().is_success() => r.json::<RemoteFileInfo>().await.ok(),
                    _ => None,
                }
            });

            if let Some(info) = updated {
                let mut metadata_cache = self.metadata_cache.lock().unwrap();
                if let Some((ino, _)) = metadata_cache.get(&path).cloned() {
                    metadata_cache.insert(path.clone(), (ino, info));
                }
            }

            Ok(())
        } else {
            Err(DOKAN_ERROR)
        }
    }

    fn set_allocation_size(
        &'h self,
        _file_name: &U16CStr,
        _alloc_size: i64,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) -> Result<(), i32> {
        Ok(())
    }

    fn set_file_attributes(
        &'h self,
        _file_name: &U16CStr,
        _file_attributes: u32,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) -> Result<(), i32> {
        Ok(())
    }

    fn set_file_time(
        &'h self,
        _file_name: &U16CStr,
        _creation_time: FileTimeOperation,
        _last_access_time: FileTimeOperation,
        _last_write_time: FileTimeOperation,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) -> Result<(), i32> {
         Ok(())
    }

    fn unlock_file(
        &'h self,
        _file_name: &U16CStr,
        _offset: i64,
        _length: i64,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) -> Result<(), i32> {
        Ok(())
    }

    fn lock_file(
        &'h self,
        _file_name: &U16CStr,
        _offset: i64,
        _length: i64,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) -> Result<(), i32> {
        Ok(())
    }

    fn get_disk_free_space(
        &'h self,
        _info: &OperationInfo<'c, 'h, Self>
    ) -> Result<dokan::DiskSpaceInfo, i32> {
        Ok(dokan::DiskSpaceInfo {
            byte_count: 2 * 1024 * 1024 * 1024,
            free_byte_count: 1024 * 1024 * 1024,
            available_byte_count: 1024 * 1024 * 1024,
        })
    }

    fn get_volume_information(
        &'h self,
        _info: &OperationInfo<'c, 'h, Self>
    ) -> Result<VolumeInfo, i32> {
         Ok(VolumeInfo {
            name: U16CString::from_str("RemoteFS").map_err(|_| DOKAN_ERROR)?,
            serial_number: 12345,
            max_component_length: 255,
            fs_flags: FILE_CASE_PRESERVED_NAMES | FILE_UNICODE_ON_DISK,
            fs_name: U16CString::from_str("NTFS").map_err(|_| DOKAN_ERROR)?, 
        })
    }

    fn mounted(
        &'h self,
        _mount_point: &U16CStr,
        _info: &OperationInfo<'c, 'h, Self>
    ) -> Result<(), i32> {
        println!("Mounted!");
        Ok(())
    }

    fn unmounted(
        &'h self,
        _info: &OperationInfo<'c, 'h, Self>
    ) -> Result<(), i32> {
        println!("Unmounted!");
        Ok(())
    }
}

pub fn run_dokany_client(filesystem: RemoteFilesystem) {
    let mount_point_path = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("mnt")
        .join("remote-fs");
    let _ = std::fs::create_dir_all(&mount_point_path);

    let mount_point = mount_point_path.to_string_lossy().to_string();
    println!("Mounting filesystem at {}", mount_point);
    
    dokan::init();

    let clicked = Arc::new(AtomicBool::new(false));
    let c = clicked.clone();

    ctrlc::set_handler(move || {
        println!("\nReceived Ctrl+C, unmounting...");
        c.store(true, Ordering::SeqCst);
        // Recompute mount point on Ctrl+C (uses current working directory).
        let mount_point_path = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join("mnt")
            .join("remote-fs");
        let mp = U16CString::from_str(mount_point_path.to_string_lossy().as_ref())
            .expect("Invalid mount point");
        let _ = dokan::unmount(mp.as_ucstr());
    }).expect("Error setting Ctrl-C handler");
    
    let mount_point_cstr = U16CString::from_str(&mount_point).expect("Invalid mount point");
    let options = MountOptions::default();
    

    let mut mounter = FileSystemMounter::new(&filesystem, mount_point_cstr.as_ucstr(), &options);
    
    match mounter.mount() {
        Ok(_) => println!("Filesystem mounted successfully."),
        Err(e) => {
            eprintln!("Failed to mount: {:?}", e);

            let err_text = format!("{:?}", e);
            if err_text.contains("DriverInstall") {
                eprintln!(
                    "\nDokan driver install/start failed (DriverInstall).\n\
                    Checks to do on Windows:\n\
                    1) Install the Dokan/Dokany driver (Dokan Library installer, x64).\n\
                    2) Reboot after installation (driver/service may not start until reboot).\n\
                    3) Run this client from an elevated (Administrator) terminal.\n\
                    4) If mounting to a directory, ensure the mount folder exists and is empty:\n\
                        {}\n\
                    5) If it still fails, Windows may be blocking the driver (signature policy / security software).\n",
                    mount_point
                );
            }
        }
    }
    
    if clicked.load(Ordering::SeqCst) {
        println!("Performing Dokan shutdown...");
        dokan::shutdown();
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
    let trimmed = path.trim_start_matches('\\').replace('\\', "/");
    if trimmed.is_empty() {
        "".to_string()
    } else {
        trimmed
    }
}