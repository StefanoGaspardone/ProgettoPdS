use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};
use crate::apis::{ApiClient, ApiError, FileEntry};

pub const CHUNK_SIZE: u32 = 128 * 1024;
const MAX_CACHE_SIZE: usize = 10 * 1024 * 1024;
pub const METADATA_CACHE_TTL: Duration = Duration::from_secs(5);
pub const DIRECTORY_CACHE_TTL: Duration = Duration::from_secs(3);
pub const DATA_CACHE_TTL: Duration = Duration::from_secs(10);

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
}

impl CacheManager {
    pub fn new() -> Self {
        Self {
            file_cache: HashMap::new(),
            directory_cache: HashMap::new(),
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

    pub fn list_directory_cached(
        &mut self,
        path: &str,
        api_client: &ApiClient,
    ) -> Result<Vec<FileEntry>, ApiError> {
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
        let chunk_start = (offset / CHUNK_SIZE as u64) * CHUNK_SIZE as u64;

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

    pub fn read_with_cache(
        &mut self,
        path: &str,
        offset: u64,
        size: u32,
        api_client: &ApiClient,
    ) -> Result<Vec<u8>, ApiError> {
        let chunk_start = (offset / CHUNK_SIZE as u64) * CHUNK_SIZE as u64;

        if let Some(data) = self.read_from_cache(path, offset, size) {
            return Ok(data);
        }

        let chunk_data = api_client.read_file_chunk(path, chunk_start, CHUNK_SIZE)?;
        self.store_file_chunk(path, chunk_start, chunk_data.clone());

        let chunk_offset = (offset - chunk_start) as usize;
        let chunk_end = (chunk_offset + size as usize).min(chunk_data.len());
        Ok(chunk_data[chunk_offset..chunk_end].to_vec())
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

        while file_cache.total_size + data.len() > MAX_CACHE_SIZE && !file_cache.chunks.is_empty() {
            if let Some((&oldest_offset, _)) = file_cache
                .chunks
                .iter()
                .min_by_key(|(_, chunk)| chunk.last_access)
            {
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

        self.file_cache
            .retain(|_, file_cache| !file_cache.chunks.is_empty());
    }

    pub fn clear(&mut self) {
        self.file_cache.clear();
        self.directory_cache.clear();
    }
}