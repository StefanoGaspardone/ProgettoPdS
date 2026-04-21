use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use crate::apis::{ApiClient, FileEntry};
use dokan::{
    CreateFileInfo, DiskSpaceInfo, FileInfo, FileSystemHandler, FileSystemMounter, FillDataResult,
    FindData, MountOptions, OperationInfo, OperationResult, VolumeInfo, IO_SECURITY_CONTEXT,
};
use libc::{EIO, ENOENT, ENOTCONN};
use widestring::{U16CStr, U16CString};

use crate::cache::{CacheManager, METADATA_CACHE_TTL};

pub type NtStatus = i32;

pub const STATUS_SUCCESS: NtStatus = 0;
pub const STATUS_OBJECT_NAME_NOT_FOUND: NtStatus = 0xC0000034u32 as i32;
pub const STATUS_IO_DEVICE_ERROR: NtStatus = 0xC0000185u32 as i32;
pub const STATUS_DEVICE_NOT_CONNECTED: NtStatus = 0xC000009Du32 as i32;
pub const STATUS_ACCESS_DENIED: NtStatus = 0xC0000022u32 as i32;
pub const STATUS_NOT_A_DIRECTORY: NtStatus = 0xC0000103u32 as i32;
pub const STATUS_FILE_IS_A_DIRECTORY: NtStatus = 0xC00000BAu32 as i32;

const FILE_NON_DIRECTORY_FILE: u32 = 0x00000040;
const FILE_DIRECTORY_FILE: u32 = 0x00000001;
const FILE_SUPERSEDE: u32 = 0;
const FILE_CREATE: u32 = 2;
const FILE_OPEN_IF: u32 = 3;
const FILE_OVERWRITE_IF: u32 = 5;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

fn posix_to_ntstatus(err: i32) -> NtStatus {
    match err {
        0 => STATUS_SUCCESS,
        ENOENT => STATUS_OBJECT_NAME_NOT_FOUND,
        ENOTCONN => STATUS_DEVICE_NOT_CONNECTED,
        EIO => STATUS_IO_DEVICE_ERROR,
        _ => STATUS_ACCESS_DENIED,
    }
}

fn normalize_remote_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches('/').to_string()
}

fn split_parent_and_name(path: &str) -> (String, String) {
    let normalized = path.trim_matches('/');
    if normalized.is_empty() {
        return ("/".to_string(), "".to_string());
    }

    if let Some((parent, name)) = normalized.rsplit_once('/') {
        let parent_norm = if parent.is_empty() {
            "/".to_string()
        } else {
            format!("/{parent}")
        };
        
        (parent_norm, name.to_string())
    } else {
        ("/".to_string(), normalized.to_string())
    }
}

struct WriteBuffer {
    offset: u64,
    data: Vec<u8>,
}

#[derive(Default)]
struct FileWriteState {
    buffer: Option<WriteBuffer>,
    handles: Vec<tokio::task::JoinHandle<Result<(), crate::apis::ApiError>>>,
}

struct DokanyFs {
    api_client: Arc<ApiClient>,
    ino_counter: Mutex<u64>,
    cache: Mutex<CacheManager>,
    last_cleanup: Mutex<Instant>,
    pending_writes: Mutex<HashMap<String, FileWriteState>>,
}

impl DokanyFs {
    fn next_ino(&self) -> u64 {
        let mut counter = self.ino_counter.lock().unwrap();
        let ino = *counter;
        
        *counter += 1;
        ino
    }

    fn lookup_entry(&self, remote_path: &str) -> Result<(u64, FileEntry), i32> {
        let normalized = remote_path.trim_start_matches('/');
        
        if normalized.is_empty() {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            
            return Ok((1, FileEntry {
                name: "/".to_string(),
                is_dir: true,
                size: 0,
                mtime: now,
                ctime: now,
                mode: 0o755,
            }));
        }

        let (parent, name) = split_parent_and_name(normalized);
        let entries = if let Ok(mut cache) = self.cache.lock() {
            cache
                .list_directory_cached(&parent, &self.api_client)
                .map_err(|e| e.errno)?
        } else {
            self.api_client.list_directory(&parent).map_err(|e| e.errno)?
        };
        
        if let Some(entry) = entries.into_iter().find(|e| e.name == name) {
            Ok((self.next_ino(), entry))
        } else {
            Err(ENOENT)
        }
    }

    fn lookup_entry_fresh(&self, remote_path: &str) -> Result<(u64, FileEntry), i32> {
        let normalized = remote_path.trim_start_matches('/');
        
        if normalized.is_empty() {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            
            return Ok((1, FileEntry {
                name: "/".to_string(),
                is_dir: true,
                size: 0,
                mtime: now,
                ctime: now,
                mode: 0o755,
            }));
        }

        let (parent, name) = split_parent_and_name(normalized);
        let entries = self.api_client.list_directory(&parent).map_err(|e| e.errno)?;

        if let Some(entry) = entries.into_iter().find(|e| e.name == name) {
            Ok((self.next_ino(), entry))
        } else {
            Err(ENOENT)
        }
    }

    fn invalidate_path(&self, path: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.invalidate_all_for_path(path);

            if let Some(stripped) = path.strip_prefix('/') {
                if !stripped.is_empty() {
                    cache.invalidate_all_for_path(stripped);
                }
            } else if !path.is_empty() {
                let absolute = format!("/{path}");
                cache.invalidate_all_for_path(&absolute);
            }
        }
    }

    fn maybe_cleanup_cache(&self) {
        if let Ok(mut last_cleanup) = self.last_cleanup.lock() && last_cleanup.elapsed() >= METADATA_CACHE_TTL {
            if let Ok(mut cache) = self.cache.lock() {
                cache.cleanup_expired();
            }
            
            *last_cleanup = Instant::now();
        }
    }
}

impl<'c, 'h> FileSystemHandler<'c, 'h> for DokanyFs where 'h: 'c {
    type Context = ();

    fn create_file(&'h self, file_name: &U16CStr, _security_context: &IO_SECURITY_CONTEXT, _desired_access: u32, _file_attributes: u32, _share_access: u32, create_disposition: u32, create_options: u32, _info: &mut OperationInfo<'c, 'h, Self>) -> OperationResult<CreateFileInfo<Self::Context>> {
        let _guard = self.api_client.enter_runtime();

        self.maybe_cleanup_cache();
        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);
        let is_dir_request = (create_options & FILE_DIRECTORY_FILE) != 0;
        let non_dir_request = (create_options & FILE_NON_DIRECTORY_FILE) != 0;

        let (parent_path, _) = split_parent_and_name(&path_str);

        match self.lookup_entry(&path_str) {
            Ok((_ino, info)) => {
                if is_dir_request && !info.is_dir {
                    return Err(STATUS_NOT_A_DIRECTORY);
                }
                
                if non_dir_request && info.is_dir {
                    return Err(STATUS_ACCESS_DENIED);
                }
                
                if create_disposition == FILE_CREATE {
                    return Err(STATUS_ACCESS_DENIED);
                }

                Ok(CreateFileInfo {
                    context: (),
                    is_dir: info.is_dir,
                    new_file_created: false,
                })
            }
            Err(ENOENT) => {
                let can_create = matches!(
                    create_disposition,
                    FILE_CREATE | FILE_OPEN_IF | FILE_OVERWRITE_IF | FILE_SUPERSEDE
                );
                
                if !can_create {
                    return Err(STATUS_OBJECT_NAME_NOT_FOUND);
                }

                if is_dir_request {
                    self.api_client
                        .create_directory(&path_str)
                        .map_err(|e| posix_to_ntstatus(e.errno))?;
                    
                    self.invalidate_path(&path_str);
                    self.invalidate_path(&parent_path);

                    return Ok(CreateFileInfo {
                        context: (),
                        is_dir: true,
                        new_file_created: true,
                    });
                }

                self.api_client
                    .write_file(&path_str, &[])
                    .map_err(|e| posix_to_ntstatus(e.errno))?;

                self.invalidate_path(&path_str);
                self.invalidate_path(&parent_path);

                Ok(CreateFileInfo {
                    context: (),
                    is_dir: false,
                    new_file_created: true,
                })
            }
            Err(e) => Err(posix_to_ntstatus(e)),
        }
    }

    fn get_file_information(&'h self, file_name: &U16CStr, _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context) -> OperationResult<FileInfo> {
        let _guard = self.api_client.enter_runtime();

        self.maybe_cleanup_cache();
        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);

        match self.lookup_entry(&path_str) {
            Ok((ino, info)) => Ok(FileInfo {
                attributes: if info.is_dir {
                    FILE_ATTRIBUTE_DIRECTORY
                } else {
                    FILE_ATTRIBUTE_NORMAL
                },
                creation_time: UNIX_EPOCH + Duration::from_secs_f64(info.ctime),
                last_access_time: UNIX_EPOCH + Duration::from_secs_f64(info.mtime),
                last_write_time: UNIX_EPOCH + Duration::from_secs_f64(info.mtime),
                file_size: info.size,
                number_of_links: 1,
                file_index: ino,
            }),
            Err(e) => Err(posix_to_ntstatus(e)),
        }
    }

    fn find_files(&'h self, file_name: &U16CStr, mut fill_find_data: impl FnMut(&FindData) -> FillDataResult, _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context) -> OperationResult<()> {
        let _guard = self.api_client.enter_runtime();

        self.maybe_cleanup_cache();
        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);
        let list_path = if path_str.is_empty() {
            "/".to_string()
        } else {
            format!("/{path_str}")
        };

        let entries = self
            .cache
            .lock()
            .ok()
            .and_then(|mut cache| cache.list_directory_cached(&list_path, &self.api_client).ok())
            .or_else(|| self.api_client.list_directory(&list_path).ok())
            .ok_or(STATUS_IO_DEVICE_ERROR)?;

        for info in entries {
            let entry = FindData {
                file_name: U16CString::from_str(&info.name).map_err(|_| STATUS_IO_DEVICE_ERROR)?,
                attributes: if info.is_dir {
                    FILE_ATTRIBUTE_DIRECTORY
                } else {
                    FILE_ATTRIBUTE_NORMAL
                },
                creation_time: UNIX_EPOCH + Duration::from_secs_f64(info.ctime),
                last_access_time: UNIX_EPOCH + Duration::from_secs_f64(info.mtime),
                last_write_time: UNIX_EPOCH + Duration::from_secs_f64(info.mtime),
                file_size: info.size,
            };

            if fill_find_data(&entry).is_err() {
                break;
            }
        }

        Ok(())
    }

    fn read_file(&'h self, file_name: &U16CStr, offset: i64, buffer: &mut [u8], _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context) -> OperationResult<u32> {
        let _guard = self.api_client.enter_runtime();

        self.maybe_cleanup_cache();
        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);

        let data = if let Ok(mut cache) = self.cache.lock() {
            cache
                .read_with_cache(&path_str, offset.max(0) as u64, buffer.len() as u32, &self.api_client)
                .map_err(|e| posix_to_ntstatus(e.errno))?
        } else {
            self.api_client
                .read_file_chunk(&path_str, offset.max(0) as u64, buffer.len() as u32)
                .map_err(|e| posix_to_ntstatus(e.errno))?
        };

        let len = data.len().min(buffer.len());
        buffer[..len].copy_from_slice(&data[..len]);
        
        Ok(len as u32)
    }

    fn write_file(&'h self, file_name: &U16CStr, offset: i64, buffer: &[u8], _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context) -> OperationResult<u32> {
        let _guard = self.api_client.enter_runtime();

        self.maybe_cleanup_cache();
        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);
        
        let offset = offset.max(0) as u64;
        let mut tasks_to_spawn = Vec::new();
        let max_buffer_size = 4 * 1024 * 1024; // 4MB buffer to hide latency
        
        let to_await = {
            let mut pending = self.pending_writes.lock().unwrap();
            let state = pending.entry(path_str.clone()).or_default();
            
            if let Some(buf) = &mut state.buffer {
                if offset == buf.offset + buf.data.len() as u64 {
                    buf.data.extend_from_slice(buffer);
                } else {
                    tasks_to_spawn.push(state.buffer.take().unwrap());
                    state.buffer = Some(WriteBuffer { offset, data: buffer.to_vec() });
                }
            } else {
                state.buffer = Some(WriteBuffer { offset, data: buffer.to_vec() });
            }
            
            if let Some(buf) = &state.buffer {
                if buf.data.len() >= max_buffer_size {
                    tasks_to_spawn.push(state.buffer.take().unwrap());
                }
            }
            
            for buf in tasks_to_spawn {
                let api_clone = self.api_client.clone();
                let path_clone = path_str.clone();
                let handle = self.api_client.spawn_task_with_handle(async move {
                    api_clone.write_file_chunk_async(&path_clone, buf.offset, buf.data).await
                });
                state.handles.push(handle);
            }
            
            let max_concurrent = 8;
            if state.handles.len() > max_concurrent {
                state.handles.drain(0..(state.handles.len() - max_concurrent / 2)).collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        };

        let mut success = true;
        if !to_await.is_empty() {
            self.api_client.block_on(async {
                for h in to_await {
                    if let Ok(Err(_)) | Err(_) = h.await {
                        success = false;
                    }
                }
            });
        }
        if !success {
            return Err(STATUS_IO_DEVICE_ERROR);
        }

        let (parent_path, _) = split_parent_and_name(&path_str);

        self.invalidate_path(&path_str);
        self.invalidate_path(&parent_path);

        Ok(buffer.len() as u32)
    }

    fn delete_file(&'h self, file_name: &U16CStr, _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context) -> OperationResult<()> {
        let _guard = self.api_client.enter_runtime();
        self.maybe_cleanup_cache();

        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);

        match self.lookup_entry_fresh(&path_str) {
            Ok((_, info)) => {
                if info.is_dir {
                    return Err(STATUS_ACCESS_DENIED); 
                }
            }
            Err(e) => return Err(posix_to_ntstatus(e)),
        }

        self.api_client
            .delete(&path_str)
            .map_err(|e| posix_to_ntstatus(e.errno))?;

        let (parent_path, _) = split_parent_and_name(&path_str);
        self.invalidate_path(&path_str);
        self.invalidate_path(&parent_path);
        
        Ok(())
    }

    fn delete_directory(&'h self, file_name: &U16CStr, _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context) -> OperationResult<()> {
        let _guard = self.api_client.enter_runtime();
        self.maybe_cleanup_cache();

        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);

        match self.lookup_entry_fresh(&path_str) {
            Ok((_, info)) => {
                if !info.is_dir {
                    // Errore: stai usando rmdir su un file normale
                    return Err(STATUS_NOT_A_DIRECTORY);
                }
            }
            Err(e) => return Err(posix_to_ntstatus(e)),
        }

        self.api_client
            .delete(&path_str)
            .map_err(|e| posix_to_ntstatus(e.errno))?;

        let (parent_path, _) = split_parent_and_name(&path_str);
        self.invalidate_path(&path_str);
        self.invalidate_path(&parent_path);

        Ok(())
    }

    fn move_file(&'h self, file_name: &U16CStr, new_file_name: &U16CStr, _replace_if_existing: bool, _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context) -> OperationResult<()> {
        let _guard = self.api_client.enter_runtime();

        self.maybe_cleanup_cache();
        let old_raw = file_name.to_string_lossy();
        let new_raw = new_file_name.to_string_lossy();
        let old_str = normalize_remote_path(&old_raw);
        let new_str = normalize_remote_path(&new_raw);

        self.api_client
            .rename(&old_str, &new_str)
            .map_err(|e| posix_to_ntstatus(e.errno))?;
        self.invalidate_path(&old_str);
        self.invalidate_path(&new_str);
        
        Ok(())
    }

    fn get_volume_information(&'h self, _info: &OperationInfo<'c, 'h, Self>) -> OperationResult<VolumeInfo> {
        Ok(VolumeInfo {
            name: U16CString::from_str("RemoteFS").map_err(|_| STATUS_IO_DEVICE_ERROR)?,
            serial_number: 12345,
            max_component_length: 255,
            fs_flags: 0,
            fs_name: U16CString::from_str("NTFS").map_err(|_| STATUS_IO_DEVICE_ERROR)?,
        })
    }

    fn get_disk_free_space(&'h self, _info: &OperationInfo<'c, 'h, Self>) -> OperationResult<DiskSpaceInfo> {
        Ok(DiskSpaceInfo {
            available_byte_count: 1024 * 1024 * 1024,
            byte_count: 1024 * 1024 * 1024,
            free_byte_count: 1024 * 1024 * 1024,
        })
    }

    fn set_end_of_file(&'h self, _file_name: &U16CStr, _offset: i64, _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context) -> OperationResult<()> {
        Ok(())
    }

    fn set_allocation_size(&'h self, _file_name: &U16CStr, _alloc_size: i64, _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context) -> OperationResult<()> {
        Ok(())
    }

    fn flush_file_buffers(&'h self, file_name: &U16CStr, _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context) -> OperationResult<()> {
        let _guard = self.api_client.enter_runtime();
        
        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);

        let handles = {
            let mut pending = self.pending_writes.lock().unwrap();
            if let Some(mut state) = pending.remove(&path_str) {
                if let Some(buf) = state.buffer.take() {
                    let api_clone = self.api_client.clone();
                    let path_clone = path_str.clone();
                    let handle = self.api_client.spawn_task_with_handle(async move {
                        api_clone.write_file_chunk_async(&path_clone, buf.offset, buf.data).await
                    });
                    state.handles.push(handle);
                }
                state.handles
            } else {
                Vec::new()
            }
        };
        
        let mut success = true;
        self.api_client.block_on(async {
            for handle in handles {
                if let Ok(Err(_)) | Err(_) = handle.await {
                    success = false;
                }
            }
        });

        if success {
            Ok(())
        } else {
            Err(STATUS_IO_DEVICE_ERROR)
        }
    }

    fn cleanup(&'h self, file_name: &U16CStr, _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context) {
        let _guard = self.api_client.enter_runtime();
        
        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);

        let handles = {
            let mut pending = self.pending_writes.lock().unwrap();
            if let Some(mut state) = pending.remove(&path_str) {
                if let Some(buf) = state.buffer.take() {
                    let api_clone = self.api_client.clone();
                    let path_clone = path_str.clone();
                    let handle = self.api_client.spawn_task_with_handle(async move {
                        api_clone.write_file_chunk_async(&path_clone, buf.offset, buf.data).await
                    });
                    state.handles.push(handle);
                }
                state.handles
            } else {
                Vec::new()
            }
        };
        
        self.api_client.block_on(async {
            for handle in handles {
                let _ = handle.await;
            }
        });
    }
}

pub fn run_dokany_client(api_client: ApiClient, mountpoint: String) -> Result<()> {
    dokan::init();

    let mountpoint = if mountpoint.ends_with(':') {
        format!("{}\\", mountpoint)
    } else {
        mountpoint
    };
    let mp = U16CString::from_str(&mountpoint)?;

    let fs = Arc::new(DokanyFs {
        api_client: Arc::new(api_client),
        ino_counter: Mutex::new(2),
        cache: Mutex::new(CacheManager::new()),
        last_cleanup: Mutex::new(Instant::now()),
        pending_writes: Mutex::new(HashMap::new()),
    });

    let mut options = MountOptions::default();
    options.single_thread = false;
    
    let mut mounter = FileSystemMounter::new(&*fs, mp.as_ucstr(), &options);
    
    log::info!("Mounting on {}...", mountpoint);

    match mounter.mount() {
        Ok(_fs_instance) => {
            log::info!("Successfully mounted on {}", mountpoint);
            log::info!("Press Ctrlc+C to unmount.");
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!("Dokany mount failed: {:?}", e)),
    }
}