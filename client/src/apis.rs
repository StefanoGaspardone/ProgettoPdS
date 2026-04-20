use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::future::Future;
use tokio::runtime::{EnterGuard, Handle};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: f64,
    pub ctime: f64,
    pub mode: u32,
}

#[derive(Debug, Deserialize)]
struct ListResponse {
    entries: Vec<FileEntry>,
}

#[derive(Serialize)]
struct SetAttrsRequest {
    mode: Option<u32>,
}

#[derive(Clone)]
pub struct ApiClient {
    base_url: String,
    client: Client,
    runtime: Handle,
}

impl ApiClient {
    fn validate_path(path: &str, operation: &str) -> Result<(), ApiError> {
        if path.split('/').any(|segment| segment == "..") {
            return Err(ApiError {
                errno: libc::EINVAL,
                message: format!("{}: Invalid path traversal attempt", operation),
            });
        }

        Ok(())
    }

    pub fn new(base_url: String, runtime: Handle) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(60))
            .tcp_nodelay(true)
            .pool_max_idle_per_host(32)
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self { base_url, client, runtime })
    }

    pub fn spawn_task<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.runtime.spawn(future);
    }

    pub fn spawn_task_with_handle<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(future)
    }

    pub async fn write_file_chunk_async(&self, path: &str, offset: u64, data: Vec<u8>) -> Result<(), ApiError> {
        Self::validate_path(path, "write_file_chunk_async")?;
        let url = format!("{}/files/{}", self.base_url, path.trim_start_matches('/'));
        
        if data.is_empty() {
            return Ok(());
        }

        let end = offset
            .checked_add(data.len() as u64 - 1)
            .ok_or_else(|| ApiError::io_error("write_file_chunk_async", "Invalid range overflow"))?;
            
        let max_retries = 4;
        let mut attempt = 0;
        
        loop {
            let response = self.client.patch(&url)
                .header("Content-Range", format!("bytes {}-{}/*", offset, end))
                .body(data.clone())
                .send()
                .await;

            match response {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_client_error() && status.as_u16() != 429 && status.as_u16() != 408 {
                        return Err(ApiError::from_status(status, "write_file_chunk_async"));
                    }
                    if attempt >= max_retries {
                        return Err(ApiError::from_status(status, "write_file_chunk_async"));
                    }
                    log::warn!("write_file_chunk_async failed (HTTP {}). Retrying ({}/{})", status, attempt + 1, max_retries);
                },
                Err(e) => {
                    if attempt >= max_retries {
                        return Err(ApiError::from_network_error("write_file_chunk_async", &e));
                    }
                    log::warn!("write_file_chunk_async network error ({}). Retrying ({}/{})", e, attempt + 1, max_retries);
                }
            }
            
            attempt += 1;
            tokio::time::sleep(tokio::time::Duration::from_millis(500 * (1 << attempt))).await;
        }
    }

    pub fn enter_runtime(&self) -> EnterGuard<'_> {
        self.runtime.enter()
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    pub fn list_directory(&self, path: &str) -> Result<Vec<FileEntry>, ApiError> {
        Self::validate_path(path, "list_directory")?;
        let url = format!("{}/list/{}", self.base_url, path.trim_start_matches('/'));
        
        log::debug!("Listing directory: {}", url);

        let response = self.block_on(
            self.client.get(&url).send()
        ).map_err(|e| ApiError::from_network_error("list_directory", &e))?;

        let status = response.status();
        if !status.is_success() {
            log::warn!("list_directory failed: HTTP {}", status);
            return Err(ApiError::from_status(status, "list_directory"));
        }

        let list_response: ListResponse = self.block_on(response.json())
            .map_err(|e| ApiError::io_error("list_directory", &format!("Failed to parse response: {}", e)))?;

        Ok(list_response.entries)
    }

    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, ApiError> {
        Self::validate_path(path, "read_file")?;
        let url = format!("{}/files/{}", self.base_url, path.trim_start_matches('/'));
        
        log::debug!("Reading file: {}", url);

        let response = self.block_on(
            self.client.get(&url).send()
        ).map_err(|e| ApiError::from_network_error("read_file", &e))?;

        let status = response.status();
        if !status.is_success() {
            log::warn!("read_file failed for {}: HTTP {}", path, status);
            return Err(ApiError::from_status(status, "read_file"));
        }

        let bytes = self.block_on(response.bytes())
            .map_err(|e| ApiError::io_error("read_file", &format!("Failed to read response: {}", e)))?;

        Ok(bytes.to_vec())
    }

    pub fn read_file_chunk(&self, path: &str, offset: u64, size: u32) -> Result<Vec<u8>, ApiError> {
        Self::validate_path(path, "read_file_chunk")?;
        
        if size == 0 {
            log::debug!("read_file_chunk called with size=0 for {}, returning empty", path);
            return Ok(Vec::new());
        }

        let url = format!(
            "{}/files/{}?offset={}&size={}",
            self.base_url,
            path.trim_start_matches('/'),
            offset,
            size
        );

        log::debug!("[API] read_file_chunk path={} offset={} size={}", path, offset, size);

        let response = self.block_on(self.client.get(&url).send())
            .map_err(|e| ApiError::from_network_error("read_file_chunk", &e))?;

        let status = response.status();
        if !status.is_success() {
            log::warn!("[API] read_file_chunk failed path={} offset={} size={} status={}", path, offset, size, status);
            return Err(ApiError::from_status(status, "read_file_chunk"));
        }

        let bytes = self.block_on(response.bytes())
            .map_err(|e| ApiError::io_error("read_file_chunk", &format!("Failed to read response: {}", e)))?;

        let result = if bytes.len() > size as usize {
            bytes[0..size as usize].to_vec()
        } else {
            bytes.to_vec()
        };

        log::debug!("[API] read_file_chunk ok path={} offset={} requested={} returned={}", path, offset, size, result.len());
        Ok(result)
    }

    pub fn write_file(&self, path: &str, data: &[u8]) -> Result<(), ApiError> {
        Self::validate_path(path, "write_file")?;
        let url = format!("{}/files/{}", self.base_url, path.trim_start_matches('/'));
        
        log::debug!("Writing file: {} ({} bytes)", url, data.len());

        let response = self.block_on(
            self.client.put(&url).body(data.to_vec()).send()
        ).map_err(|e| ApiError::from_network_error("write_file", &e))?;

        let status = response.status();
        if !status.is_success() {
            log::warn!("write_file failed for {}: HTTP {}", path, status);
            return Err(ApiError::from_status(status, "write_file"));
        }

        Ok(())
    }

    pub fn write_file_chunk(&self, path: &str, offset: u64, data: &[u8]) -> Result<(), ApiError> {
        Self::validate_path(path, "write_file_chunk")?;
        let url = format!("{}/files/{}", self.base_url, path.trim_start_matches('/'));
        
        log::debug!("Writing file chunk: {} (offset={}, size={})", url, offset, data.len());

        if data.is_empty() {
            log::debug!("write_file_chunk called with empty payload for {}, nothing to write", path);
            return Ok(());
        }

        let end = offset
            .checked_add(data.len() as u64 - 1)
            .ok_or_else(|| ApiError::io_error("write_file_chunk", "Invalid range overflow"))?;
        let response = self.block_on(
            self.client.patch(&url)
                .header("Content-Range", format!("bytes {}-{}/*", offset, end))
                .body(data.to_vec())
                .send()
        ).map_err(|e| ApiError::from_network_error("write_file_chunk", &e))?;

        let status = response.status();
        if status.is_success() {
            log::debug!("Successfully wrote chunk using PATCH");
            return Ok(());
        }

        if status.as_u16() == 405 {
            log::warn!("Server doesn't support PATCH, falling back to read-modify-write");
            return self.write_file_chunk_fallback(path, offset, data);
        }

        log::warn!("write_file_chunk failed for {}: HTTP {}", path, status);
        Err(ApiError::from_status(status, "write_file_chunk"))
    }

    fn write_file_chunk_fallback(&self, path: &str, offset: u64, data: &[u8]) -> Result<(), ApiError> {
        log::warn!("Using inefficient read-modify-write for {}", path);

        let mut file_data = match self.read_file(path) {
            Ok(existing) => existing,
            Err(err) if err.errno == libc::ENOENT => Vec::new(),
            Err(err) => {
                log::warn!("Fallback read failed for {}: {}", path, err);
                return Err(err);
            }
        };

        let end_offset = (offset as usize) + data.len();
        if end_offset > file_data.len() {
            file_data.resize(end_offset, 0);
        }

        file_data[offset as usize..end_offset].copy_from_slice(data);
        self.write_file(path, &file_data)
    }

    pub fn create_directory(&self, path: &str) -> Result<(), ApiError> {
        Self::validate_path(path, "create_directory")?;
        let url = format!("{}/mkdir/{}", self.base_url, path.trim_start_matches('/'));
        
        log::debug!("Creating directory: {}", url);

        let response = self.block_on(
            self.client.post(&url).send()
        ).map_err(|e| ApiError::from_network_error("create_directory", &e))?;

        let status = response.status();
        if !status.is_success() {
            log::warn!("create_directory failed for {}: HTTP {}", path, status);
            return Err(ApiError::from_status(status, "create_directory"));
        }

        Ok(())
    }

    pub fn delete(&self, path: &str) -> Result<(), ApiError> {
        Self::validate_path(path, "delete")?;
        let url = format!("{}/files/{}", self.base_url, path.trim_start_matches('/'));
        
        log::debug!("Deleting: {}", url);

        let response = self.block_on(
            self.client.delete(&url).send()
        ).map_err(|e| ApiError::from_network_error("delete", &e))?;

        let status = response.status();
        if !status.is_success() {
            log::warn!("delete failed for {}: HTTP {}", path, status);
            return Err(ApiError::from_status(status, "delete"));
        }

        Ok(())
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<(), ApiError> {
        Self::validate_path(from, "rename")?;
        Self::validate_path(to, "rename")?;
        
        let url = format!("{}/rename", self.base_url);
        
        log::debug!("Renaming: {} -> {}", from, to);

        #[derive(Serialize)]
        struct RenameRequest {
            from: String,
            to: String,
        }

        let request_body = RenameRequest {
            from: from.to_string(),
            to: to.to_string(),
        };

        let response = self.block_on(
            self.client.post(&url).json(&request_body).send()
        ).map_err(|e| ApiError::from_network_error("rename", &e))?;

        let status = response.status();
        if !status.is_success() {
            log::warn!("rename failed ({} -> {}): HTTP {}", from, to, status);
            return Err(ApiError::from_status(status, "rename"));
        }

        Ok(())
    }

    pub fn set_attrs(&self, path: &str, mode: Option<u32>) -> Result<(), ApiError> {
        Self::validate_path(path, "set_attrs")?;
        let url = format!("{}/attrs/{}", self.base_url, path.trim_start_matches('/'));
        
        log::debug!("Setting attrs for {}: mode={:?}", url, mode);

        let request_body = SetAttrsRequest { mode };

        let response = self.block_on(
            self.client.patch(&url).json(&request_body).send()
        ).map_err(|e| ApiError::from_network_error("set_attrs", &e))?;

        let status = response.status();
        if !status.is_success() {
            log::warn!("set_attrs failed for {}: HTTP {}", path, status);
            return Err(ApiError::from_status(status, "set_attrs"));
        }

        Ok(())
    }

    pub fn health_check(&self) -> Result<(), ApiError> {
        let url = format!("{}/health", self.base_url);

        let response = self.block_on(
            self.client.get(&url).send()
        ).map_err(|e| ApiError::from_network_error("health_check", &e))?;

        let status = response.status();
        if !status.is_success() {
            return Err(ApiError::from_status(status, "health_check"));
        }

        Ok(())
    }
}

#[derive(Debug)]
pub struct ApiError {
    pub errno: i32,
    pub message: String,
}

impl ApiError {
    pub fn from_status(status: reqwest::StatusCode, operation: &str) -> Self {
        let (errno, message) = match status.as_u16() {
            400 => (libc::EINVAL, format!("{}: Bad request", operation)),
            401 | 403 => (libc::EACCES, format!("{}: Permission denied", operation)),
            404 => (libc::ENOENT, format!("{}: File or directory not found", operation)),
            405 => (libc::ENOSYS, format!("{}: Operation not supported", operation)),
            408 => (libc::ETIMEDOUT, format!("{}: Request timeout", operation)),
            409 => {
                if operation == "delete" {
                    (libc::ENOTEMPTY, format!("{}: Directory not empty", operation))
                } else {
                    (libc::EEXIST, format!("{}: Resource already exists", operation))
                }
            },
            413 => (libc::EFBIG, format!("{}: File too large", operation)),
            415 => (libc::EINVAL, format!("{}: Unsupported media type", operation)),
            429 => (libc::EAGAIN, format!("{}: Too many requests", operation)),

            500 => (libc::EIO, format!("{}: Internal server error", operation)),
            501 => (libc::ENOSYS, format!("{}: Not implemented", operation)),
            502 | 503 => (libc::EAGAIN, format!("{}: Service unavailable", operation)),
            504 => (libc::ETIMEDOUT, format!("{}: Gateway timeout", operation)),
            507 => (libc::ENOSPC, format!("{}: Insufficient storage", operation)),

            _ => (libc::EIO, format!("{}: HTTP error {}", operation, status.as_u16())),
        };

        Self { errno, message }
    }

    pub fn from_network_error(operation: &str, err: &reqwest::Error) -> Self {
        let (errno, message) = if err.is_timeout() {
            (libc::ETIMEDOUT, format!("{}: Connection timeout", operation))
        } else if err.is_connect() {
            (libc::EHOSTUNREACH, format!("{}: Cannot connect to server", operation))
        } else {
            (libc::EIO, format!("{}: Network error: {}", operation, err))
        };

        Self { errno, message }
    }

    pub fn io_error(operation: &str, detail: &str) -> Self {
        Self {
            errno: libc::EIO,
            message: format!("{}: {}", operation, detail),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (errno: {})", self.message, self.errno)
    }
}

impl std::error::Error for ApiError {}