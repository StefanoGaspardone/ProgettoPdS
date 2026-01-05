# WinFSP Implementation for Remote Filesystem Client

## Overview

This document describes the Windows filesystem support implementation using WinFSP (Windows File System Proxy) for the Remote Filesystem Client. The implementation mirrors the Linux FUSE implementation while adapting to Windows-specific APIs and conventions.

## Architecture

### Core Components

1. **FileSystemContext Implementation** (`winfsp.rs`)
   - Implements the `winfsp::filesystem::FileSystemContext` trait
   - Uses inode-based file tracking (similar to FUSE)
   - Manages filesystem operations through HTTP requests to remote server

2. **Remote Filesystem Structure** (`main.rs`)
   - `RemoteFilesystem`: Main struct containing:
     - `server_url`: HTTP endpoint for remote filesystem
     - `runtime`: Tokio async runtime for network operations
     - `metadata_cache`: Caches file metadata locally
     - `inode_cache`: Maps inode numbers to file paths
     - `next_inode`: Counter for generating unique inode numbers

## Implemented Operations

### File Operations

#### `open()` - Open/Create File
- Looks up file in local cache or remote server
- Creates new file/directory if needed
- Returns inode for file context

```rust
fn open(&mut self, file_name: &str, create_options: u32, granted_access: u32) 
    -> Result<Self::FileContext>
```

#### `close()` - Close File
- Cleanup operation when file handle is closed
- No special operations needed

#### `read()` - Read File Data
- Fetches file content from remote server via `/files/{path}` endpoint
- Supports partial reads with offset and size parameters
- Returns data slice or appropriate error

```rust
fn read(&mut self, file_context: Self::FileContext, offset: u64, size: u32) 
    -> Result<Vec<u8>>
```

#### `write()` - Write File Data
- Sends file content to remote server via PUT request to `/files/{path}`
- Updates local metadata cache with new file size
- Returns number of bytes written

```rust
fn write(&mut self, file_context: Self::FileContext, offset: u64, buffer: &[u8]) 
    -> Result<u32>
```

#### `delete_file()` - Delete File
- Sends DELETE request to `/files/{path}` endpoint
- Removes file from local metadata and inode caches
- Handles both files and directories

```rust
fn delete_file(&mut self, file_context: Self::FileContext) -> Result<()>
```

### Directory Operations

#### `read_directory()` - List Directory Contents
- Fetches directory listing from `/list/{path}` endpoint
- Returns vector of directory entries with file info
- Adds special entries (`.` and `..`) for navigation

```rust
fn read_directory(&mut self, file_context: Self::FileContext) 
    -> Result<Vec<(String, FileInfo)>>
```

#### `create_directory()` - Create Directory
- Sends POST request to `/mkdir/{path}` endpoint
- Creates new inode and caches directory metadata
- Returns file context and directory info

```rust
fn create_directory(&mut self, file_name: &str) 
    -> Result<(Self::FileContext, FileInfo)>
```

### Metadata Operations

#### `get_file_info()` - Get File Attributes
- Retrieves cached file information by inode
- Returns Windows FILETIME format timestamps
- Handles file attributes (directory vs regular file)

```rust
fn get_file_info(&self, name: &str) -> Option<(Self::FileContext, FileInfo)>
```

#### `set_file_size()` - Resize File
- Updates file size in metadata cache
- Returns updated file info with new allocation size

```rust
fn set_file_size(&mut self, file_context: Self::FileContext, new_size: u64) 
    -> Result<FileInfo>
```

### Volume Operations

#### `get_volume_info()` - Get Volume Information
- Returns volume capacity and label
- Reports 1 TB total and free space (configurable)
- Provides volume label "RemoteFS"

```rust
fn get_volume_info(&self) -> Result<VolumeInfo>
```

## Data Structures

### FileInfo (WinFSP Format)
```rust
struct FileInfo {
    attributes: u32,              // File attributes (0x10 = DIR, 0x80 = NORMAL)
    creation_time: u64,           // Windows FILETIME format
    last_access_time: u64,        // Windows FILETIME format
    last_write_time: u64,         // Windows FILETIME format
    change_time: u64,             // Windows FILETIME format
    allocation_size: u64,         // Size in 4KB blocks
    file_size: u64,               // Actual file size in bytes
    hard_links: u32,              // Number of hard links (usually 1)
    reparse_tag: u32,             // For symbolic links (unused)
    index_number: u64,            // Inode number
    ea_size: u32,                 // Extended attributes size (unused)
}
```

## Time Format Conversion

WinFSP uses Windows FILETIME format (100-nanosecond intervals since 1601-01-01):
```rust
fn unix_to_filetime(unix_secs: u64) -> u64 {
    unix_secs * 10_000_000 + 116_444_736_000_000_000
}
```

## Path Normalization

Paths are normalized from Windows format (`\`) to forward slash format (`/`):
```rust
fn normalize_path(path: &str) -> String {
    path.replace("\\", "/")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string()
}
```

## HTTP API Integration

The implementation communicates with a remote HTTP server using these endpoints:

| Operation | Method | Endpoint | Purpose |
|-----------|--------|----------|---------|
| Get file metadata | GET | `/stat/{path}` | Retrieve file info |
| Read file | GET | `/files/{path}` | Download file content |
| Write file | PUT | `/files/{path}` | Upload file content |
| Delete file/dir | DELETE | `/files/{path}` | Remove file |
| List directory | GET | `/list/{path}` | Get directory contents |
| Create directory | POST | `/mkdir/{path}` | Create new directory |

## Error Handling

WinFSP errors are mapped to Windows NT status codes:
- `2`: File not found (STATUS_NO_SUCH_FILE)
- `5`: Access denied (STATUS_ACCESS_DENIED)
- `31`: Device error (STATUS_DEVICE_IO_ERROR)

## Mount Point

The filesystem is mounted at `M:` drive on Windows. This can be configured in the `run_winfsp_client()` function.

## Threading and Concurrency

- All filesystem operations run on the WinFSP thread pool
- Interior mutability (Mutex) is used for shared state:
  - `metadata_cache`: HashMap of path → (inode, FileInfo)
  - `inode_cache`: HashMap of inode → path
- Network operations use Tokio async runtime

## Comparison with FUSE Implementation

| Feature | FUSE | WinFSP |
|---------|------|--------|
| OS | Linux | Windows |
| Mount point | `mnt/remote-fs` | `M:` |
| Inode base | 1 | 1 |
| Time format | UNIX timestamp | Windows FILETIME |
| Path format | `/` separator | `\` separator (normalized to `/`) |
| Attributes | Unix permissions | Windows attributes |
| API | High-level fuser crate | Low-level winfsp crate |

## Key Features Implemented

✅ File read/write operations
✅ Directory listing and creation
✅ File creation and deletion
✅ Directory deletion
✅ File metadata caching
✅ Remote HTTP integration
✅ Proper timestamp conversion
✅ Path normalization
✅ Error handling with appropriate Windows NT status codes
✅ Volume information reporting
✅ Graceful shutdown with Ctrl+C

## Future Enhancements

- [ ] Rename operations
- [ ] Extended attributes support
- [ ] Security/permissions handling
- [ ] Sparse file support
- [ ] Symbolic link support
- [ ] Async/concurrent file operations optimization
- [ ] Configurable mount point
- [ ] Connection pooling for HTTP requests
