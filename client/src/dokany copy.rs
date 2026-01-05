use dokan::{
    CreateFileInfo, FileInfo, FileSystemHandler, FileSystemMounter, FileTimeOperation, FillDataResult, FindData, MountFlags, MountOptions, OperationInfo, VolumeInfo
};
use dokan_sys::DOKAN_IO_SECURITY_CONTEXT;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use widestring::{U16CStr, U16CString, U16CStr as UCString}; // UCString is not needed, using U16CStr
use crate::{RemoteFilesystem, FileInfo as RemoteFileInfo};

// Constants for File Attributes (Win32)
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x00000010;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x00000080;
const FILE_CASE_SENSITIVE_SEARCH: u32 = 0x00000001;
const FILE_CASE_PRESERVED_NAMES: u32 = 0x00000002;
const FILE_UNICODE_ON_DISK: u32 = 0x00000004;

// Access Rights
const FILE_WRITE_DATA: u32 = 0x00000002;
const FILE_APPEND_DATA: u32 = 0x00000004;
const GENERIC_WRITE: u32 = 0x40000000;
const DELETE: u32 = 0x00010000;

// Generic error code (NTSTATUS -1 is generic error)
const DOKAN_ERROR: i32 = -1073741823; // STATUS_UNSUCCESSFUL
const STATUS_OBJECT_NAME_NOT_FOUND: i32 = -1073741772;
const STATUS_OBJECT_NAME_COLLISION: i32 = -1073741771;
const STATUS_NOT_A_DIRECTORY: i32 = -1073741565;

const FILE_DIRECTORY_FILE: u32 = 0x00000001;
const CREATE_NEW: u32 = 1;
const CREATE_ALWAYS: u32 = 2;
const OPEN_EXISTING: u32 = 3;
const OPEN_ALWAYS: u32 = 4;
const TRUNCATE_EXISTING: u32 = 5;

impl<'c, 'h> FileSystemHandler<'c, 'h> for RemoteFilesystem 
where 'h: 'c
{
    type Context = ();

    fn create_file(
        &'h self,
        file_name: &U16CStr,
        _security_context: &DOKAN_IO_SECURITY_CONTEXT,
        access_mode: u32,
        _file_attributes: u32,
        _share_access: u32,
        create_disposition: u32,
        create_options: u32,
        _info: &mut OperationInfo<'c, 'h, Self>
    ) -> Result<CreateFileInfo<Self::Context>, i32> {
        let path = normalize_path(&file_name.to_string_lossy());
        if path.is_empty() {
             return Ok(CreateFileInfo {
                context: (),
                is_dir: true,
                new_file_created: false,
            });
        }
        println!("CreateFile: Path='{}', Disposition={}, Access={:X}", 
        path, create_disposition, access_mode);
        let mut metadata_cache = self.metadata_cache.lock().unwrap();

        // 1. Check if the file exists (Cache first, then Server)
        let existing_info = if let Some((_, info)) = metadata_cache.get(&path) {
            Some(info.clone())
        } else {
            // Release lock for network IO
            drop(metadata_cache);

            let url = self.server_url.join(&format!("/stat/{}", path)).map_err(|_| DOKAN_ERROR)?;
            let remote_info = self.runtime.block_on(async {
                let client = reqwest::Client::new();
                let res = client.get(url).send().await;
                match res {
                    Ok(r) if r.status().is_success() => r.json::<RemoteFileInfo>().await.ok(),
                    _ => None,
                }
            });

            // Re-acquire lock
            metadata_cache = self.metadata_cache.lock().unwrap();

            if let Some((_, info)) = metadata_cache.get(&path) {
                Some(info.clone())
            } else if let Some(info) = remote_info {
                let mut next_inode = self.next_inode.lock().unwrap();
                let ino = *next_inode;
                *next_inode += 1;
                metadata_cache.insert(path.clone(), (ino, info.clone()));
                Some(info)
            } else {
                None
            }
        };

        // 2. Handle based on existence and disposition
        match existing_info {
            Some(info) => {
                if create_disposition == CREATE_NEW {
                    return Err(STATUS_OBJECT_NAME_COLLISION);
                }
                
                if (create_options & FILE_DIRECTORY_FILE) != 0 && info.file_type != "dir" {
                    return Err(STATUS_NOT_A_DIRECTORY);
                }

                let mut should_truncate = false;
                if info.file_type == "file" {
                    if create_disposition == CREATE_ALWAYS || create_disposition == TRUNCATE_EXISTING {
                        should_truncate = true;
                    }
                }

                if should_truncate {
                    drop(metadata_cache);
                    let success = self.runtime.block_on(async {
                        let client = reqwest::Client::new();
                        let url = self.server_url.join(&format!("/files/{}", path)).ok()?;
                        let res = client.put(url)
                            .header("Content-Type", "application/octet-stream")
                            .body(Vec::new())
                            .send()
                            .await;
                        Some(matches!(res, Ok(r) if r.status().is_success()))
                    }).unwrap_or(false);

                    metadata_cache = self.metadata_cache.lock().unwrap();

                    if success {
                        if let Some((_, info)) = metadata_cache.get_mut(&path) {
                            info.size = 0;
                        }
                    } else {
                        return Err(DOKAN_ERROR);
                    }
                }

                Ok(CreateFileInfo {
                    context: (),
                    is_dir: info.file_type == "dir",
                    new_file_created: false,
                })
            }
            None => {
                if create_disposition == OPEN_EXISTING || create_disposition == TRUNCATE_EXISTING {
                    return Err(STATUS_OBJECT_NAME_NOT_FOUND);
                }

                let is_dir_request = (create_options & FILE_DIRECTORY_FILE) != 0;
                let is_write_intent = (access_mode & (GENERIC_WRITE | FILE_WRITE_DATA | FILE_APPEND_DATA)) != 0;
                let is_delete_intent = (access_mode & DELETE) != 0;
                let is_system_probe = is_system_file(&path);

                let should_create = match create_disposition {
                    CREATE_NEW | CREATE_ALWAYS => !is_system_probe && !is_delete_intent,
                    OPEN_ALWAYS => is_write_intent && !is_system_probe && !is_delete_intent,
                    _ => false,
                };

                if !should_create {
                    return Err(STATUS_OBJECT_NAME_NOT_FOUND);
                }

                drop(metadata_cache);

                let server_res = self.runtime.block_on(async {
                    let client = reqwest::Client::new();
                    if is_dir_request {
                        let url = self.server_url.join(&format!("/mkdir/{}", path)).ok()?;
                        let res = client.post(url).send().await.ok()?;
                        Some((res.status().is_success(), res.status().as_u16() == 409))
                    } else {
                        let url = self.server_url.join(&format!("/files/{}", path)).ok()?;
                        let res = client.put(url)
                            .header("Content-Type", "application/octet-stream")
                            .body(Vec::new())
                            .send().await.ok()?;
                        Some((res.status().is_success(), res.status().as_u16() == 409))
                    }
                }).unwrap_or((false, false));

                metadata_cache = self.metadata_cache.lock().unwrap();

                if server_res.1 && create_disposition == CREATE_NEW {
                    return Err(STATUS_OBJECT_NAME_COLLISION);
                }

                if server_res.0 {
                    let mut next_inode = self.next_inode.lock().unwrap();
                    let ino = *next_inode;
                    *next_inode += 1;

                    let new_info = RemoteFileInfo {
                        name: path.split('/').last().unwrap_or("").to_string(),
                        path: path.clone(),
                        file_type: if is_dir_request { "dir".to_string() } else { "file".to_string() },
                        size: 0,
                        timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
                        permissions: "755".to_string(),
                    };

                    metadata_cache.insert(path.clone(), (ino, new_info));
                    Ok(CreateFileInfo {
                        context: (),
                        is_dir: is_dir_request,
                        new_file_created: true,
                    })
                } else {
                    Err(DOKAN_ERROR)
                }
            }
        }
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
        _offset: i64,
        buffer: &[u8],
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) -> Result<u32, i32> {
        let path_str = file_name.to_string_lossy();
        let path = normalize_path(&path_str);

        println!("Writing file: {}", path);
        
        let url = self.server_url.join(&format!("/files/{}", path)).map_err(|_| DOKAN_ERROR)?;
        
        let success = self.runtime.block_on(async {
            let client = reqwest::Client::new();
            let res = client.put(url)
                .header("Content-Type", "application/octet-stream")
                .body(buffer.to_vec())
                .send()
                .await;
            matches!(res, Ok(r) if r.status().is_success())
        });

        if success {
             Ok(buffer.len() as u32)
        } else {
             Err(DOKAN_ERROR)
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
        file_name: &U16CStr,
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
        _file_name: &U16CStr,
        _offset: i64,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context
    ) -> Result<(), i32> {
        Ok(())
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
    let mount_point = "M:";
    println!("Mounting filesystem at {}", mount_point);
    
    dokan::init();

    let clicked = Arc::new(AtomicBool::new(false));
    let c = clicked.clone();

    ctrlc::set_handler(move || {
        println!("\nReceived Ctrl+C, unmounting...");
        c.store(true, Ordering::SeqCst);
        let mp = U16CString::from_str("M:").expect("Invalid mount point");
        dokan::unmount(mp.as_ucstr());
    }).expect("Error setting Ctrl-C handler");
    
    let mount_point_cstr = U16CString::from_str(mount_point).expect("Invalid mount point");
    let mut options = MountOptions::default();
    

    let mut mounter = FileSystemMounter::new(&filesystem, mount_point_cstr.as_ucstr(), &options);
    
    match mounter.mount() {
        Ok(_) => println!("Filesystem mounted successfully."),
        Err(e) => eprintln!("Failed to mount: {:?}", e),
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