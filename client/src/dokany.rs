use std::sync::Arc;
use std::time::SystemTime;
use dokan::{
    FileSystemHandler, OperationInfo, OperationResult, IO_SECURITY_CONTEXT,
    FileInfo, FindData, VolumeInfo, DiskSpaceInfo, CreateFileInfo,
    MountOptions, FileSystemMounter, FillDataResult
};
use widestring::{U16CStr, U16CString};
use libc::{ENOENT, EIO, ENOTCONN};
use crate::RemoteFilesystem;

pub type NTSTATUS = i32;

pub const STATUS_SUCCESS: NTSTATUS = 0;
pub const STATUS_OBJECT_NAME_NOT_FOUND: NTSTATUS = 0xC0000034u32 as i32;
pub const STATUS_IO_DEVICE_ERROR: NTSTATUS = 0xC0000185u32 as i32;
pub const STATUS_DEVICE_NOT_CONNECTED: NTSTATUS = 0xC000009Du32 as i32;
pub const STATUS_ACCESS_DENIED: NTSTATUS = 0xC0000022u32 as i32;
pub const STATUS_NOT_IMPLEMENTED: NTSTATUS = 0xC0000002u32 as i32;
pub const STATUS_NOT_A_DIRECTORY: NTSTATUS = 0xC0000103u32 as i32;

const FILE_NON_DIRECTORY_FILE: u32 = 0x00000040;
const FILE_DIRECTORY_FILE: u32 = 0x00000001;
const FILE_SUPERSEDE: u32 = 0;
const FILE_CREATE: u32 = 2;
const FILE_OPEN_IF: u32 = 3;
const FILE_OVERWRITE_IF: u32 = 5;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

fn posix_to_ntstatus(err: i32) -> NTSTATUS {
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

impl<'c, 'h> FileSystemHandler<'c, 'h> for RemoteFilesystem where 'h: 'c {
    type Context = ();

    fn create_file(&'h self, file_name: &U16CStr, _security_context: &IO_SECURITY_CONTEXT, _desired_access: u32, _file_attributes: u32, _share_access: u32, create_disposition: u32, create_options: u32, _info: &mut OperationInfo<'c, 'h, Self>) -> OperationResult<CreateFileInfo<Self::Context>> {
        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);
        let is_dir_request = (create_options & FILE_DIRECTORY_FILE) != 0;
        let non_dir_request = (create_options & FILE_NON_DIRECTORY_FILE) != 0;

        match self.runtime_handle.block_on(self.get_stat(&path_str)) {
            Ok((_ino, info)) => {
                let is_dir = info.file_type == "dir";

                if is_dir_request && !is_dir {
                    return Err(STATUS_NOT_A_DIRECTORY);
                }

                if non_dir_request && is_dir {
                    return Err(STATUS_ACCESS_DENIED);
                }

                if create_disposition == FILE_CREATE {
                    return Err(STATUS_ACCESS_DENIED);
                }

                Ok(CreateFileInfo {
                    context: (),
                    is_dir,
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
                    self.runtime_handle
                        .block_on(self.create_dir(&path_str))
                        .map_err(posix_to_ntstatus)?;

                    return Ok(CreateFileInfo {
                        context: (),
                        is_dir: true,
                        new_file_created: true,
                    });
                }

                if non_dir_request || !is_dir_request {
                    self.runtime_handle
                        .block_on(self.write_file(&path_str, 0, Vec::new()))
                        .map_err(posix_to_ntstatus)?;

                    return Ok(CreateFileInfo {
                        context: (),
                        is_dir: false,
                        new_file_created: true,
                    });
                }

                Err(STATUS_ACCESS_DENIED)
            }
            Err(e) => Err(posix_to_ntstatus(e)),
        }
    }

    fn get_file_information(&'h self, file_name: &U16CStr, _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context,) -> OperationResult<FileInfo> {
        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);
        
        match self.runtime_handle.block_on(self.get_stat(&path_str)) {
            Ok((ino, info)) => {
                let is_dir = info.file_type == "dir";
                
                Ok(FileInfo {
                    attributes: if is_dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL },
                    creation_time: SystemTime::UNIX_EPOCH,
                    last_access_time: SystemTime::UNIX_EPOCH,
                    last_write_time: SystemTime::UNIX_EPOCH,
                    file_size: info.size,
                    number_of_links: 1,
                    file_index: ino as u64,
                })
            }
            Err(e) => Err(posix_to_ntstatus(e)),
        }
    }

    fn find_files(&'h self, file_name: &U16CStr, mut fill_find_data: impl FnMut(&FindData) -> FillDataResult, _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context,) -> OperationResult<()> {
        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);
        
        match self.runtime_handle.block_on(self.list_dir(&path_str)) {
            Ok(entries) => {
                for (_ino, info) in entries {
                    let is_dir = info.file_type == "dir";
                    let entry = FindData {
                        file_name: U16CString::from_str(&info.name).unwrap(),
                        attributes: if is_dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL },
                        creation_time: SystemTime::UNIX_EPOCH,
                        last_access_time: SystemTime::UNIX_EPOCH,
                        last_write_time: SystemTime::UNIX_EPOCH,
                        file_size: info.size,
                    };

                    if fill_find_data(&entry).is_err() { break; }
                }
                Ok(())
            }
            Err(e) => Err(posix_to_ntstatus(e)),
        }
    }

    fn read_file(&'h self, file_name: &U16CStr, offset: i64, buffer: &mut [u8], _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context,) -> OperationResult<u32> {
        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);
        
        match self.runtime_handle.block_on(self.read_file(&path_str, offset as u64, buffer.len() as u32)) {
            Ok(data) => {
                let len = data.len();
                buffer[..len].copy_from_slice(&data);
                
                Ok(len as u32)
            }
            Err(e) => Err(posix_to_ntstatus(e)),
        }
    }

    fn write_file(&'h self, file_name: &U16CStr, offset: i64, buffer: &[u8], _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context,) -> OperationResult<u32> {
        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);
        
        match self.runtime_handle.block_on(self.write_file(&path_str, offset as u64, buffer.to_vec())) {
            Ok(_) => Ok(buffer.len() as u32),
            Err(e) => Err(posix_to_ntstatus(e)),
        }
    }

    fn delete_file(&'h self, file_name: &U16CStr, _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context) -> OperationResult<()> {
        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);
        
        match self.runtime_handle.block_on(self.delete_path(&path_str)) {
            Ok(_) => Ok(()),
            Err(e) => Err(posix_to_ntstatus(e)),
        }
    }

    fn delete_directory(&'h self, file_name: &U16CStr, _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context,) -> OperationResult<()> {
        let path_raw = file_name.to_string_lossy();
        let path_str = normalize_remote_path(&path_raw);
        
        match self.runtime_handle.block_on(self.delete_path(&path_str)) {
            Ok(_) => Ok(()),
            Err(e) => Err(posix_to_ntstatus(e)),
        }
    }

    fn move_file(&'h self, file_name: &U16CStr, new_file_name: &U16CStr, _replace_if_existing: bool, _info: &OperationInfo<'c, 'h, Self>, _context: &'c Self::Context) -> OperationResult<()> {
        let old_raw = file_name.to_string_lossy();
        let new_raw = new_file_name.to_string_lossy();
        let old_str = normalize_remote_path(&old_raw);
        let new_str = normalize_remote_path(&new_raw);
        
        match self.runtime_handle.block_on(self.rename(&old_str, &new_str)) {
            Ok(_) => Ok(()),
            Err(e) => Err(posix_to_ntstatus(e)),
        }
    }

    fn get_volume_information(&'h self, _info: &OperationInfo<'c, 'h, Self>) -> OperationResult<VolumeInfo> {
        Ok(VolumeInfo {
            name: U16CString::from_str("RemoteFS").unwrap(),
            serial_number: 12345,
            max_component_length: 255,
            fs_flags: 0, 
            fs_name: U16CString::from_str("NTFS").unwrap(),
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

    fn mounted(&'h self, _mount_point: &U16CStr, _info: &OperationInfo<'c, 'h, Self>) -> OperationResult<()> {
        println!("[DOKAN] File System successfully mounted!");
        Ok(())
    }
}

pub fn run_dokany_client(fs: Arc<RemoteFilesystem>, mountpoint: String) {
    dokan::init(); 

    let mp = match U16CString::from_str(&mountpoint) {
        Ok(mp) => mp,
        Err(_) => {
            eprintln!("Invalid mountpoint");
            return;
        }
    };
    
    let mut options = MountOptions::default();
    options.single_thread = false;
    
    let mut mounter = FileSystemMounter::new(&*fs, mp.as_ucstr(), &options);
    
    println!("[DOKAN] Mounting on {}...", mountpoint);
    
    match mounter.mount() {
        Ok(_) => {
        }
        Err(e) => {
            eprintln!("[DOKAN] Error during execution: {:?}", e);
        }
    }
    
    dokan::shutdown();
}