# WinFSP FileSystemContext Trait Implementation Guide

## Overview
The `winfsp` crate requires implementing the `FileSystemContext` trait to create a Windows filesystem driver. This guide provides the key trait definition and method signatures you need.

---

## 1. Required Associated Type

```rust
type FileContext: Sized
```
- Represents an open handle in the file system
- Your context type (e.g., `u64` for inode)
- Used throughout all filesystem operations
- Only accessible through shared (`&`) references due to threading constraints
- Use interior mutability (like `Arc<Mutex<T>>`) if you need to mutate state

---

## 2. Required Methods (Must Implement)

### `get_security_by_name`
```rust
fn get_security_by_name(
    &self,
    file_name: &U16CStr,
    security_descriptor: Option<&mut [c_void]>,
    reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
) -> Result<FileSecurity>
```
- Get security information and attributes for a file by its name
- Called before `open()` to check permissions
- Must handle reparse points if supported

### `open`
```rust
fn open(
    &self,
    file_name: &U16CStr,
    create_options: u32,
    granted_access: FILE_ACCESS_RIGHTS,
    file_info: &mut OpenFileInfo,
) -> Result<Self::FileContext>
```
- Opens a file or directory
- Populates `file_info` with file metadata
- Returns your FileContext (e.g., inode handle)
- `create_options` contains disposition flags (e.g., FILE_DIRECTORY_FILE)

### `close`
```rust
fn close(&self, context: Self::FileContext)
```
- Closes a file or directory handle
- Clean up resources associated with the context

---

## 3. Provided Methods (Optional Overrides)

### File Creation & Deletion

#### `create`
```rust
fn create(
    &self,
    file_name: &U16CStr,
    create_options: u32,
    granted_access: FILE_ACCESS_RIGHTS,
    file_attributes: FILE_FLAGS_AND_ATTRIBUTES,
    security_descriptor: Option<&[c_void]>,
    allocation_size: u64,
    extra_buffer: Option<&[u8]>,
    extra_buffer_is_reparse_point: bool,
    file_info: &mut OpenFileInfo,
) -> Result<Self::FileContext>
```
- Create a new file or directory
- Default implementation calls `open()` if file exists, otherwise creates and opens

#### `cleanup`
```rust
fn cleanup(
    &self,
    context: &Self::FileContext,
    file_name: Option<&U16CStr>,
    flags: u32,
)
```
- Clean up a file after all handles are closed
- Set delete flag here; actual deletion happens when all handles close

#### `set_delete`
```rust
fn set_delete(
    &self,
    context: &Self::FileContext,
    file_name: &U16CStr,
    delete_file: bool,
) -> Result<()>
```
- Mark file for deletion (actual deletion in `cleanup()`)

### File Information

#### `get_file_info`
```rust
fn get_file_info(
    &self,
    context: &Self::FileContext,
    file_info: &mut FileInfo,
) -> Result<()>
```
- Get current file information (size, times, attributes)
- Populate the `FileInfo` struct with metadata

#### `get_security`
```rust
fn get_security(
    &self,
    context: &Self::FileContext,
    security_descriptor: Option<&mut [c_void]>,
) -> Result<u64>
```
- Get security descriptor for open file handle
- Returns size of security descriptor

#### `set_security`
```rust
fn set_security(
    &self,
    context: &Self::FileContext,
    security_information: u32,
    modification_descriptor: ModificationDescriptor,
) -> Result<()>
```
- Set security descriptor on file

### File Operations

#### `read`
```rust
fn read(
    &self,
    context: &Self::FileContext,
    buffer: &mut [u8],
    offset: u64,
) -> Result<u32>  // Returns bytes read
```
- Read from file at given offset
- Fill the provided buffer, return actual bytes read

#### `write`
```rust
fn write(
    &self,
    context: &Self::FileContext,
    buffer: &[u8],
    offset: u64,
    write_to_eof: bool,
    constrained_io: bool,
    file_info: &mut FileInfo,
) -> Result<u32>  // Returns bytes written
```
- Write to file at given offset
- Update file_info with new size/times
- Return actual bytes written

#### `flush`
```rust
fn flush(
    &self,
    context: Option<&Self::FileContext>,
    file_info: &mut FileInfo,
) -> Result<()>
```
- Flush file or volume
- If context is None, flush entire volume

### File Size & Attributes

#### `set_file_size`
```rust
fn set_file_size(
    &self,
    context: &Self::FileContext,
    new_size: u64,
    set_allocation_size: bool,
    file_info: &mut FileInfo,
) -> Result<()>
```
- Truncate or extend file size
- Update allocation size if requested

#### `set_basic_info`
```rust
fn set_basic_info(
    &self,
    context: &Self::FileContext,
    file_attributes: u32,
    creation_time: u64,
    last_access_time: u64,
    last_write_time: u64,
    last_change_time: u64,
    file_info: &mut FileInfo,
) -> Result<()>
```
- Set file attributes and timestamps
- Times are in Windows FILETIME format (100-nanosecond intervals)

#### `overwrite`
```rust
fn overwrite(
    &self,
    context: &Self::FileContext,
    file_attributes: FILE_FLAGS_AND_ATTRIBUTES,
    replace_file_attributes: bool,
    allocation_size: u64,
    extra_buffer: Option<&[u8]>,
    file_info: &mut FileInfo,
) -> Result<()>
```
- Overwrite existing file with new attributes

### Directory Operations

#### `read_directory`
```rust
fn read_directory(
    &self,
    context: &Self::FileContext,
    pattern: Option<&U16CStr>,
    marker: DirMarker,
    buffer: &mut [u8],
) -> Result<u32>  // Returns bytes written to buffer
```
- Enumerate directory entries matching pattern
- Use `marker` to track enumeration position
- Write entries to buffer using `WideNameInfo` trait

#### `get_dir_info_by_name`
```rust
fn get_dir_info_by_name(
    &self,
    context: &Self::FileContext,
    file_name: &U16CStr,
    out_dir_info: &mut DirInfo,
) -> Result<()>
```
- Get information about single file within directory

#### `rename`
```rust
fn rename(
    &self,
    context: &Self::FileContext,
    file_name: &U16CStr,
    new_file_name: &U16CStr,
    replace_if_exists: bool,
) -> Result<()>
```
- Rename or move file

### Volume Operations

#### `get_volume_info`
```rust
fn get_volume_info(
    &self,
    out_volume_info: &mut VolumeInfo,
) -> Result<()>
```
- Get volume information (capacity, free space, etc.)

#### `set_volume_label`
```rust
fn set_volume_label(
    &self,
    volume_label: &U16CStr,
    volume_info: &mut VolumeInfo,
) -> Result<()>
```
- Set or change volume label

### Reparse Points (Optional)

#### `get_reparse_point_by_name`
```rust
fn get_reparse_point_by_name(
    &self,
    file_name: &U16CStr,
    is_directory: bool,
    buffer: &mut [u8],
) -> Result<u64>  // Returns size
```

#### `set_reparse_point` / `delete_reparse_point`
```rust
fn set_reparse_point(...) -> Result<()>
fn delete_reparse_point(...) -> Result<()>
```

### Extended Attributes & Streams (Optional)

#### `get_extended_attributes` / `set_extended_attributes`
```rust
fn get_extended_attributes(...) -> Result<u32>
fn set_extended_attributes(...) -> Result<()>
```

#### `get_stream_info`
```rust
fn get_stream_info(
    &self,
    context: &Self::FileContext,
    buffer: &mut [u8],
) -> Result<u32>
```

### Control & Lifecycle

#### `control`
```rust
fn control(
    &self,
    context: &Self::FileContext,
    control_code: u32,
    input: &[u8],
    output: &mut [u8],
) -> Result<u32>
```
- Handle DeviceIoControl calls

#### `dispatcher_stopped`
```rust
fn dispatcher_stopped(&self, normally: bool)
```
- Called when filesystem dispatcher stops
- Do user-mode only cleanup (not kernel-mode)

---

## Key Data Structures

### `FileInfo` (File Metadata)
```rust
pub struct FileInfo {
    pub file_attributes: u32,      // FILE_ATTRIBUTE_* flags
    pub reparse_tag: u32,          // Reparse point tag if applicable
    pub allocation_size: u64,      // Allocated bytes (usually cluster-aligned)
    pub file_size: u64,            // Actual file size in bytes
    pub creation_time: u64,        // Windows FILETIME (100-ns intervals)
    pub last_access_time: u64,     // Windows FILETIME
    pub last_write_time: u64,      // Windows FILETIME
    pub change_time: u64,          // Windows FILETIME
    pub index_number: u64,         // File reference number (usually inode)
    pub hard_links: u32,           // Number of hard links (unimplemented, use 0)
    pub ea_size: u32,              // Extended attributes size (usually 0)
}
```

### `OpenFileInfo`
```rust
pub struct OpenFileInfo {
    // Private fields - access via AsRef/AsMut<FileInfo>
}

// Access file info:
let file_info: &FileInfo = open_file_info.as_ref();
let file_info_mut: &mut FileInfo = open_file_info.as_mut();

// Methods:
open_file_info.set_normalized_name(&name_bytes, prefix);
open_file_info.normalized_name_size() -> u16;
```

### `VolumeInfo` (Volume Metadata)
```rust
pub struct VolumeInfo {
    pub total_size: u64,           // Total bytes in volume
    pub free_size: u64,            // Free bytes available
}
```

### `DirInfo<const BUFFER_SIZE: usize = 255>`
```rust
pub struct DirInfo<const BUFFER_SIZE: usize = 255> {
    // Private fields
}

// Methods:
dir_info.new() -> Self;
dir_info.file_info_mut() -> &mut FileInfo;

// Implement WideNameInfo trait to set name:
dir_info.set_name("filename")?;           // From OsStr
dir_info.set_name_cstr(&u16c_str)?;       // From U16CStr
dir_info.set_name_raw(&[u16; ...])?;      // From raw u16 bytes
dir_info.reset();
```

### `FileSecurity`
```rust
pub struct FileSecurity {
    // Represents security descriptor for a file
}
```

### `ModificationDescriptor`
```rust
pub struct ModificationDescriptor {
    // Internal pointer to security descriptor
}
```

---

## Time Conversion Helpers

Windows FILETIME is 100-nanosecond intervals since January 1, 1601:

```rust
use std::time::{SystemTime, UNIX_EPOCH, Duration};

// Convert SystemTime to Windows FILETIME:
fn to_windows_filetime(st: SystemTime) -> u64 {
    const EPOCH_DIFF: u64 = 116444736000000000; // 100-ns intervals from 1601-1970
    let duration = st.duration_since(UNIX_EPOCH).unwrap();
    duration.as_nanos() as u64 / 100 + EPOCH_DIFF
}

// Convert Windows FILETIME to SystemTime:
fn from_windows_filetime(ft: u64) -> SystemTime {
    const EPOCH_DIFF: u64 = 116444736000000000;
    let duration = Duration::from_nanos((ft - EPOCH_DIFF) * 100);
    UNIX_EPOCH + duration
}
```

---

## Important Notes

### Thread Safety
- `FileContext` is only accessible via `&` (shared references)
- Any mutable state must use interior mutability (Mutex, RwLock, etc.)
- Methods can be called from multiple threads concurrently

### String Handling
- Use `U16CStr` for Windows wide strings
- Convert via `to_string()` or `to_os_string()`
- Remember UTF-16 encoding

### File Attributes
Common FILE_ATTRIBUTE_* constants:
- `FILE_ATTRIBUTE_READONLY` (0x1)
- `FILE_ATTRIBUTE_HIDDEN` (0x2)
- `FILE_ATTRIBUTE_SYSTEM` (0x4)
- `FILE_ATTRIBUTE_ARCHIVE` (0x20)
- `FILE_ATTRIBUTE_NORMAL` (0x80)
- `FILE_ATTRIBUTE_DIRECTORY` (0x10)
- `FILE_ATTRIBUTE_REPARSE_POINT` (0x400)

### Error Handling
Return `Result<T>` which translates to NT status codes:
- `Ok(value)` → STATUS_SUCCESS
- `Err(NtStatus::from_win32(code))` → Windows error code
- `Err(NtStatus::NOT_FOUND)` → File not found
- `Err(NtStatus::ACCESS_DENIED)` → Permission denied

---

## Basic Implementation Template

```rust
use winfsp::filesystem::*;

pub struct MyFileSystem {
    // Your storage backend
}

impl FileSystemContext for MyFileSystem {
    type FileContext = u64; // e.g., inode

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        security_descriptor: Option<&mut [c_void]>,
        reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> Result<FileSecurity> {
        // Check if reparse point exists
        if let Some(security) = reparse_point_resolver(file_name) {
            return Ok(security);
        }
        // Your implementation
        Ok(FileSecurity::default())
    }

    fn open(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        granted_access: u32,
        file_info: &mut OpenFileInfo,
    ) -> Result<Self::FileContext> {
        // Find or create file, populate file_info
        Ok(inode)
    }

    fn close(&self, context: Self::FileContext) {
        // Clean up context if needed
    }
}
```

---

## References
- [WinFSP Docs](https://docs.rs/winfsp/)
- [WinFSP-Sys Low-level bindings](https://docs.rs/winfsp-sys/)
- Windows File System API documentation
