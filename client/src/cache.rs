use std::collections::HashMap;
use std::env;
use std::time::{Duration, Instant, SystemTime};
use crate::apis::{ApiClient, ApiError, FileEntry};

pub const CHUNK_SIZE: u32 = 1024 * 1024;
const MAX_CACHE_SIZE: usize = 10 * 1024 * 1024;
const MIN_CHUNK_SIZE: u32 = 64 * 1024;
const MAX_CHUNK_SIZE: u32 = 8 * 1024 * 1024;
const MIN_CACHE_SIZE: usize = 4 * 1024 * 1024;
const MAX_CACHE_SIZE_LIMIT: usize = 1024 * 1024 * 1024;
pub const METADATA_CACHE_TTL: Duration = Duration::from_secs(5);
pub const DIRECTORY_CACHE_TTL: Duration = Duration::from_secs(3);
pub const DATA_CACHE_TTL: Duration = Duration::from_secs(10);

fn configured_chunk_size() -> u32 {
    const ENV_VAR: &str = "REMOTEFS_CHUNK_SIZE_KB";

    match env::var(ENV_VAR) {
        Ok(raw) => match raw.trim().parse::<u32>() {
            Ok(kb) if kb > 0 => {
                let requested = kb.saturating_mul(1024);
                let effective = requested.clamp(MIN_CHUNK_SIZE, MAX_CHUNK_SIZE);

                if requested != effective {
                    log::warn!(
                        "{}={} out of range; clamped to {} KiB",
                        ENV_VAR,
                        kb,
                        effective / 1024
                    );
                }

                effective
            }
            _ => {
                log::warn!(
                    "Invalid {}='{}'; using default {} KiB",
                    ENV_VAR,
                    raw,
                    CHUNK_SIZE / 1024
                );
                CHUNK_SIZE
            }
        },
        Err(_) => CHUNK_SIZE,
    }
}

fn configured_max_cache_size() -> usize {
    const ENV_VAR: &str = "REMOTEFS_CACHE_SIZE_MB";

    match env::var(ENV_VAR) {
        Ok(raw) => match raw.trim().parse::<usize>() {
            Ok(mb) if mb > 0 => {
                let requested = mb.saturating_mul(1024 * 1024);
                let effective = requested.clamp(MIN_CACHE_SIZE, MAX_CACHE_SIZE_LIMIT);

                if requested != effective {
                    log::warn!(
                        "{}={} out of range; clamped to {} MiB",
                        ENV_VAR,
                        mb,
                        effective / (1024 * 1024)
                    );
                }

                effective
            }
            _ => {
                log::warn!(
                    "Invalid {}='{}'; using default {} MiB",
                    ENV_VAR,
                    raw,
                    MAX_CACHE_SIZE / (1024 * 1024)
                );
                MAX_CACHE_SIZE
            }
        },
        Err(_) => MAX_CACHE_SIZE,
    }
}

#[derive(Debug, Clone)]
pub struct CachedEntry<T> {
    pub data: T,
    created_at: Instant,
    ttl: Duration,
}

impl<T> CachedEntry<T> {
    pub fn new(data: T, ttl: Duration) -> Self {
        Self {
            data,
            created_at: Instant::now(),
            ttl,
        }
    }

    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() > self.ttl
    }
}

#[derive(Debug, Clone)]
struct CachedChunk {
    data: Vec<u8>,
    last_access: SystemTime,
    created_at: Instant,
}

impl CachedChunk {
    fn is_expired(&self) -> bool {
        self.created_at.elapsed() > DATA_CACHE_TTL
    }
}

#[derive(Debug, Clone)]
struct FileCache {
    chunks: HashMap<u64, CachedChunk>,
    total_size: usize,
}

#[derive(Debug, Clone)]
pub struct DirectoryCache {
    pub entries: CachedEntry<Vec<FileEntry>>,
}

pub struct CacheManager {
    file_cache: HashMap<String, FileCache>,
    directory_cache: HashMap<String, DirectoryCache>,
    chunk_size: u32,
    max_cache_size: usize,
}

impl CacheManager {
    pub fn new() -> Self {
        let chunk_size = configured_chunk_size();
        let max_cache_size = configured_max_cache_size();

        log::info!(
            "Cache tuning: chunk_size={} KiB max_cache={} MiB",
            chunk_size / 1024,
            max_cache_size / (1024 * 1024)
        );

        Self {
            file_cache: HashMap::new(),
            directory_cache: HashMap::new(),
            chunk_size,
            max_cache_size,
        }
    }

    pub fn get_cached_directory(&mut self, path: &str) -> Option<Vec<FileEntry>> {
        if let Some(dir_cache) = self.directory_cache.get(path) {
            if !dir_cache.entries.is_expired() {
                return Some(dir_cache.entries.data.clone());
            }
        }

        self.directory_cache.remove(path);
        None
    }

    pub fn store_directory_listing(&mut self, path: &str, entries: Vec<FileEntry>) {
        self.directory_cache.insert(
            path.to_string(),
            DirectoryCache {
                entries: CachedEntry::new(entries, DIRECTORY_CACHE_TTL),
            },
        );
    }

    pub fn list_directory_cached(&mut self, path: &str, api_client: &ApiClient) -> Result<Vec<FileEntry>, ApiError> {
        if let Some(entries) = self.get_cached_directory(path) {
            return Ok(entries);
        }

        let entries = api_client.list_directory(path)?;
        self.store_directory_listing(path, entries.clone());
        
        Ok(entries)
    }

    pub fn invalidate_directory_cache(&mut self, path: &str) {
        self.directory_cache.remove(path);

        if let Some(parent_pos) = path.rfind('/') {
            let parent = if parent_pos == 0 { "/" } else { &path[..parent_pos] };
            self.directory_cache.remove(parent);
        }
    }

    pub fn read_from_cache(&mut self, path: &str, offset: u64, size: u32) -> Option<Vec<u8>> {
        let chunk_size = self.chunk_size as u64;
        let chunk_start = (offset / chunk_size) * chunk_size;

        if let Some(file_cache) = self.file_cache.get_mut(path) {
            if let Some(cached_chunk) = file_cache.chunks.get_mut(&chunk_start) {
                if !cached_chunk.is_expired() {
                    cached_chunk.last_access = SystemTime::now();
                    
                    let chunk_offset = (offset - chunk_start) as usize;
                    let chunk_end = (chunk_offset + size as usize).min(cached_chunk.data.len());
                    
                    return Some(cached_chunk.data[chunk_offset..chunk_end].to_vec());
                }

                if let Some(removed) = file_cache.chunks.remove(&chunk_start) {
                    file_cache.total_size = file_cache.total_size.saturating_sub(removed.data.len());
                }
            }
        }

        None
    }

    pub fn store_file_chunk(&mut self, path: &str, offset: u64, data: Vec<u8>) {
        self.store_chunk(path, offset, data);
    }

    pub fn read_with_cache(&mut self, path: &str, offset: u64, size: u32, api_client: &ApiClient) -> Result<Vec<u8>, ApiError> {
        if size == 0 {
            return Ok(Vec::new());
        }

        let chunk_size = self.chunk_size;
        let mut remaining = size as usize;
        let mut current_offset = offset;
        let mut result = Vec::with_capacity(size as usize);

        while remaining > 0 {
            let chunk_start = (current_offset / chunk_size as u64) * chunk_size as u64;
            let chunk_offset = (current_offset - chunk_start) as usize;
            let needed_from_chunk = (chunk_size as usize - chunk_offset).min(remaining);

            let chunk_data = if let Some(data) = self.read_from_cache(path, current_offset, needed_from_chunk as u32) {
                data
            } else {
                let data = api_client.read_file_chunk(path, chunk_start, chunk_size)?;

                if data.is_empty() {
                    break;
                }

                if chunk_offset >= data.len() {
                    break;
                }

                let end = (chunk_offset + needed_from_chunk).min(data.len());
                let sliced = data[chunk_offset..end].to_vec();
                self.store_file_chunk(path, chunk_start, data);
                sliced
            };

            if chunk_data.is_empty() {
                break;
            }

            let read_len = chunk_data.len();
            result.extend_from_slice(&chunk_data);
            current_offset += read_len as u64;
            remaining = remaining.saturating_sub(read_len);

            if read_len < needed_from_chunk {
                break;
            }
        }

        Ok(result)
    }

    fn store_chunk(&mut self, path: &str, offset: u64, data: Vec<u8>) {
        let file_cache = self
            .file_cache
            .entry(path.to_string())
            .or_insert_with(|| FileCache {
                chunks: HashMap::new(),
                total_size: 0,
            });

        let expired_offsets: Vec<u64> = file_cache
            .chunks
            .iter()
            .filter(|(_, chunk)| chunk.is_expired())
            .map(|(&chunk_offset, _)| chunk_offset)
            .collect();

        for expired_offset in expired_offsets {
            if let Some(removed) = file_cache.chunks.remove(&expired_offset) {
                file_cache.total_size = file_cache.total_size.saturating_sub(removed.data.len());
            }
        }

        while file_cache.total_size + data.len() > self.max_cache_size && !file_cache.chunks.is_empty() {
            if let Some((&oldest_offset, _)) = file_cache
                .chunks
                .iter()
                .min_by_key(|(_, chunk)| chunk.last_access) {
                if let Some(removed) = file_cache.chunks.remove(&oldest_offset) {
                    file_cache.total_size = file_cache.total_size.saturating_sub(removed.data.len());
                }
            } else {
                break;
            }
        }

        let chunk_size = data.len();
        if let Some(previous) = file_cache.chunks.remove(&offset) {
            file_cache.total_size = file_cache.total_size.saturating_sub(previous.data.len());
        }

        file_cache.chunks.insert(
            offset,
            CachedChunk {
                data,
                last_access: SystemTime::now(),
                created_at: Instant::now(),
            },
        );
        file_cache.total_size += chunk_size;
    }

    pub fn invalidate_file_cache(&mut self, path: &str) {
        self.file_cache.remove(path);
    }

    pub fn invalidate_all_for_path(&mut self, path: &str) {
        self.invalidate_file_cache(path);
        self.invalidate_directory_cache(path);
    }

    pub fn cleanup_expired(&mut self) {
        self.directory_cache
            .retain(|_, dir_cache| !dir_cache.entries.is_expired());

        for file_cache in self.file_cache.values_mut() {
            let mut reclaimed = 0usize;
            file_cache.chunks.retain(|_, chunk| {
                let keep = !chunk.is_expired();
                
                if !keep {
                    reclaimed += chunk.data.len();
                }
                
                keep
            });
            
            file_cache.total_size = file_cache.total_size.saturating_sub(reclaimed);
        }

        self.file_cache.retain(|_, file_cache| !file_cache.chunks.is_empty());
    }

    pub fn clear(&mut self) {
        self.file_cache.clear();
        self.directory_cache.clear();
    }
}