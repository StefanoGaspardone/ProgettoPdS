use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::time::Duration;
use tokio::runtime::Runtime;
use reqwest::{Url, Client};
use serde::{Serialize, Deserialize};
use moka::future::Cache;
use libc::{ECONNREFUSED, EIO, ENOENT, ENOTCONN, ETIMEDOUT};
use std::collections::HashMap;
use std::cmp::min;
use tokio::sync::RwLock;

#[cfg(target_os = "windows")]
pub mod dokany;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod fuser;

// --- MODEL ---

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    pub file_type: String,
    pub size: u64,
    pub timestamp: u64,
    pub permissions: String,
}

impl FileInfo {
    pub fn root() -> Self {
        Self {
            name: "".into(),
            path: "".into(),
            file_type: "dir".into(),
            size: 0,
            timestamp: 0,
            permissions: "755".into(),
        }
    }
}

#[derive(Serialize)]
struct RenameRequest {
    pub new_path: String,
}

// --- CORE REMOTE FILESYSTEM ---

pub struct RemoteFilesystem {
    pub server_url: Url,
    pub http_client: Client,
    pub runtime: Runtime,
    pub metadata_cache: Cache<String, (u64, FileInfo)>,
    pub read_cache: Cache<String, Arc<Vec<u8>>>,
    pub path_to_inode: RwLock<HashMap<String, u64>>,
    pub inode_to_path: RwLock<HashMap<u64, String>>,
    pub parent_map: RwLock<HashMap<u64, u64>>,
    pub is_online: Arc<AtomicBool>,
    next_inode: AtomicU64,
}

impl RemoteFilesystem {
    const READ_CHUNK_SIZE: usize = 1024 * 1024; // 1 MiB

    fn endpoint_for_path(prefix: &str, path_clean: &str) -> String {
        if path_clean.is_empty() {
            prefix.to_string()
        } else {
            format!("{}/{}", prefix, path_clean)
        }
    }

    async fn fetch_range(&self, path_clean: &str, offset: u64, size: usize) -> Result<Vec<u8>, i32> {
        let url_str = format!("files/{}?offset={}&size={}", path_clean, offset, size);
        let url = self.server_url.join(&url_str).map_err(|_| EIO)?;

        let resp = self.http_client.get(url)
            .send()
            .await
            .map_err(map_net_error)?;

        if !resp.status().is_success() {
            return Err(EIO);
        }

        let bytes = resp.bytes().await.map_err(map_net_error)?;
        Ok(bytes.to_vec())
    }

    async fn fetch_chunk(&self, path_clean: &str, chunk_start: u64) -> Result<Arc<Vec<u8>>, i32> {
        let chunk_key = format!("{}:{}", path_clean, chunk_start);

        if let Some(cached) = self.read_cache.get(&chunk_key).await {
            return Ok(cached);
        }

        let data = self.fetch_range(path_clean, chunk_start, Self::READ_CHUNK_SIZE).await?;
        let chunk = Arc::new(data);
        self.read_cache.insert(chunk_key, chunk.clone()).await;
        Ok(chunk)
    }

    pub fn new(server_url: &str) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let url = Url::parse(server_url)?;
        
        let http_client = Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(2))
            .build()?;
        
        let runtime = Runtime::new()?;
        let is_online = Arc::new(AtomicBool::new(true));

        let metadata_cache = Cache::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(5)) // 5 secs
            .build();

        let read_cache = Cache::builder()
            .max_capacity(256) // 256 chunks
            .time_to_live(Duration::from_secs(20))
            .build();

        let fs = Arc::new(Self {
            server_url: url,
            http_client,
            runtime,
            metadata_cache,
            read_cache,
            path_to_inode: RwLock::new(HashMap::new()),
            inode_to_path: RwLock::new(HashMap::new()),
            parent_map: RwLock::new(HashMap::from([(1, 1)])),
            next_inode: AtomicU64::new(2),
            is_online: is_online.clone(),
        });
        
        let fs_check = fs.clone();
        tokio::spawn(async move {
            let health_url = fs_check.server_url.join("health").unwrap();
            let mut was_online = true; 

            loop {
                let res = fs_check.http_client.get(health_url.clone()).send().await;
                let now_online = res.is_ok() && res.unwrap().status().is_success();
                
                if !was_online && now_online {
                    fs_check.metadata_cache.invalidate_all();
                    fs_check.read_cache.invalidate_all();
                    
                    println!("[HEALTH] Server back online: caches cleared for consistency.");
                }
                
                fs_check.is_online.store(now_online, Ordering::SeqCst);
                
                was_online = now_online;

                let sleep_duration = if now_online { 5 } else { 2 };
                tokio::time::sleep(Duration::from_secs(sleep_duration)).await;
            }
        });

        Ok(fs)
    }

    pub async fn get_allocated_inode(&self, path: &str) -> u64 {
        let path_clean = path.trim_start_matches('/');
        
        if let Some(&ino) = self.path_to_inode.read().await.get(path_clean) {
            return ino;
        }

        let mut p2i = self.path_to_inode.write().await;
        let mut i2p = self.inode_to_path.write().await;
        
        if let Some(&ino) = p2i.get(path_clean) {
            return ino;
        }

        let ino = self.next_inode.fetch_add(1, Ordering::SeqCst);
        p2i.insert(path_clean.to_string(), ino);
        i2p.insert(ino, path_clean.to_string());
        
        ino
    }

    // --- API methods ---

    pub async fn get_stat(&self, path: &str) -> Result<(u64, FileInfo), i32> {
        if !self.is_online.load(Ordering::SeqCst) {
            return Err(ENOTCONN); 
        }

        let path_clean = path.trim_start_matches('/');
        let ino = self.get_allocated_inode(path_clean).await;

        if let Some((_, cached)) = self.metadata_cache.get(path_clean).await {
            return Ok((ino, cached));
        }

        let endpoint = Self::endpoint_for_path("stat", path_clean);
        let url = self.server_url.join(&endpoint).map_err(|_| EIO)?;
        let resp = self.http_client.get(url).send().await.map_err(map_net_error)?;
        if resp.status() == 404 { return Err(ENOENT); }
        
        let info: FileInfo = resp.json().await.map_err(map_net_error)?;
        
        self.metadata_cache.insert(path_clean.to_string(), (ino, info.clone())).await;
        Ok((ino, info))
    }

    pub async fn list_dir(&self, path: &str) -> Result<Vec<(u64, FileInfo)>, i32> {
        if !self.is_online.load(Ordering::SeqCst) {
            return Err(ENOTCONN); 
        }

        let path_clean = path.trim_start_matches('/');
        let endpoint = Self::endpoint_for_path("list", path_clean);
        let url = self.server_url.join(&endpoint).map_err(|_| EIO)?;
        let resp = self.http_client.get(url).send().await.map_err(map_net_error)?;
        let files: Vec<FileInfo> = resp.json().await.map_err(map_net_error)?;

        let mut result = Vec::with_capacity(files.len());
        for file in files {
            let ino = self.get_allocated_inode(&file.path).await;
            self.metadata_cache.insert(file.path.clone(), (ino, file.clone())).await;
            result.push((ino, file));
        }

        Ok(result)
    }

    pub async fn read_file(&self, path: &str, offset: u64, size: u32) -> Result<Vec<u8>, i32> {
        if !self.is_online.load(Ordering::SeqCst) {
            return Err(ENOTCONN); 
        }

        let path_clean = path.trim_start_matches('/');
        
        if size == 0 {
            return Ok(Vec::new());
        }

        let requested = size as usize;
        
        let mut out = Vec::with_capacity(requested);
        let mut remaining = requested;
        let mut current_offset = offset;

        while remaining > 0 {
            let chunk_start = (current_offset / Self::READ_CHUNK_SIZE as u64) * Self::READ_CHUNK_SIZE as u64;
            let offset_in_chunk = (current_offset - chunk_start) as usize;
            
            let chunk = match self.fetch_chunk(path_clean, chunk_start).await {
                Ok(cached) => cached,
                Err(_) => {
                    self.read_cache.invalidate_all();
                    return self.fetch_range(path_clean, offset, requested).await;
                }
            };

            if offset_in_chunk >= chunk.len() {
                break;
            }

            let to_take = min(remaining, chunk.len() - offset_in_chunk);
            out.extend_from_slice(&chunk[offset_in_chunk..offset_in_chunk + to_take]);

            remaining -= to_take;
            current_offset += to_take as u64;

            if to_take == 0 || chunk.len() < Self::READ_CHUNK_SIZE {
                break;
            }
        }

        Ok(out)
    }

    pub async fn write_file(&self, path: &str, offset: u64, data: Vec<u8>) -> Result<(), i32> {
        if !self.is_online.load(Ordering::SeqCst) {
            return Err(ENOTCONN); 
        }

        let path_clean = path.trim_start_matches('/');
        let url_str = format!("files/{}?offset={}", path_clean, offset);
        let url = self.server_url.join(&url_str).map_err(|_| EIO)?;

        let resp = self.http_client.put(url)
            .body(data)
            .send()
            .await
            .map_err(map_net_error)?;

        if resp.status().is_success() {
            self.metadata_cache.remove(path_clean).await;
            self.read_cache.invalidate_all();
            
            Ok(())
        } else {
            Err(EIO)
        }
    }

    pub async fn create_dir(&self, path: &str) -> Result<(), i32> {
        if !self.is_online.load(Ordering::SeqCst) {
            return Err(ENOTCONN); 
        }

        let path_clean = path.trim_start_matches('/');
        let url = self.server_url.join(&format!("mkdir/{}", path_clean)).map_err(|_| EIO)?;

        let resp = self.http_client.post(url).send().await.map_err(map_net_error)?;
        if resp.status().is_success() { Ok(()) } else { Err(EIO) }
    }

    pub async fn delete_path(&self, path: &str) -> Result<(), i32> {
        if !self.is_online.load(Ordering::SeqCst) {
            return Err(ENOTCONN); 
        }

        let path_clean = path.trim_start_matches('/');
        let url = self.server_url.join(&format!("files/{}", path_clean)).map_err(|_| EIO)?;

        let resp = self.http_client.delete(url).send().await.map_err(map_net_error)?;
        if resp.status().is_success() || resp.status() == 204 {
            self.metadata_cache.remove(path_clean).await;
            self.read_cache.invalidate_all();
            
            Ok(())
        } else {
            Err(EIO)
        }
    }

    pub async fn rename(&self, old_path: &str, new_path: &str) -> Result<(), i32> {
        if !self.is_online.load(Ordering::SeqCst) {
            return Err(ENOTCONN); 
        }

        let old_clean = old_path.trim_start_matches('/');
        let new_clean = new_path.trim_start_matches('/');
        
        let url = self.server_url.join(&format!("rename/{}", old_clean)).map_err(|_| EIO)?;

        let resp = self.http_client.post(url)
            .json(&RenameRequest { new_path: new_clean.to_string() })
            .send().await.map_err(map_net_error)?;

        if resp.status().is_success() {
            if let Some((ino, info)) = self.metadata_cache.get(old_clean).await {
                let mut new_info = info.clone();
                new_info.path = new_clean.to_string();
                
                self.metadata_cache.remove(old_clean).await;
                self.metadata_cache.insert(new_clean.to_string(), (ino, new_info)).await;
            } else {
                self.metadata_cache.remove(old_clean).await;
            }

            let mut p2i = self.path_to_inode.write().await;
            let mut i2p = self.inode_to_path.write().await;
            
            if let Some(ino) = p2i.remove(old_clean) {
                p2i.insert(new_clean.to_string(), ino);
                i2p.insert(ino, new_clean.to_string());
            }

            self.read_cache.invalidate_all();

            Ok(())
        } else {
            Err(EIO)
        }
    }

    // --- UTILS ---

    pub async fn get_path_by_ino(&self, ino: u64) -> Option<String> {
        self.inode_to_path.read().await.get(&ino).cloned()
    }
}

fn map_net_error(e: reqwest::Error) -> i32 {
    if e.is_timeout() { ETIMEDOUT }
    else if e.is_connect() { ECONNREFUSED }
    else { EIO }
}