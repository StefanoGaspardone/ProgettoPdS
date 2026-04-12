use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::time::Duration;
use std::sync::OnceLock;
use tokio::runtime::{Handle, Runtime};
use reqwest::{Url, Client};
use serde::{Serialize, Deserialize};
use moka::future::Cache;
use libc::{ECONNREFUSED, EIO, ENOENT, ENOTCONN, ETIMEDOUT};
use std::collections::HashMap;
use std::cmp::min;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

fn global_runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| Runtime::new().expect("failed to create tokio runtime"))
}

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
    pub runtime_handle: Handle,
    pub metadata_cache: Cache<String, (u64, FileInfo)>,
    pub read_cache: Cache<String, Arc<Vec<u8>>>,
    pub fetching_chunks: Arc<Mutex<std::collections::HashSet<String>>>,
    pub path_to_inode: RwLock<HashMap<String, u64>>,
    pub inode_to_path: RwLock<HashMap<u64, String>>,
    pub parent_map: RwLock<HashMap<u64, u64>>,
    pub is_online: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    health_task: Mutex<Option<JoinHandle<()>>>,
    next_inode: AtomicU64,
}

impl RemoteFilesystem {
    const READ_CHUNK_SIZE: usize = 8 * 1024 * 1024; // 8 MiB chunk for maximum speed

    fn endpoint_for_path(prefix: &str, path_clean: &str) -> String {
        if path_clean.is_empty() {
            prefix.to_string()
        } else {
            format!("{}/{}", prefix, path_clean)
        }
    }

    async fn fetch_range(&self, path_clean: &str, offset: u64, size: usize) -> Result<Vec<u8>, i32> {
        println!("[DEBUG] fetch_range() requesting -> {} | offset: {} | size: {}", path_clean, offset, size);

        let url_str = format!("files/{}?offset={}&size={}", path_clean, offset, size);
        let url = self.server_url.join(&url_str).map_err(|_| EIO)?;

        let resp = self.http_client.get(url)
            .send()
            .await
            .map_err(map_net_error)?;

        if !resp.status().is_success() {
            println!("[ERROR] fetch_range() failed with status {}", resp.status());
            return Err(EIO);
        }

        let bytes = resp.bytes().await.map_err(map_net_error)?;
        println!("[DEBUG] fetch_range() downloaded bytes: {}", bytes.len());
        Ok(bytes.to_vec())
    }

    async fn fetch_chunk(&self, path_clean: &str, chunk_start: u64) -> Result<Arc<Vec<u8>>, i32> {
        let chunk_key = format!("{}:{}", path_clean, chunk_start);
        
        loop {
            if let Some(cached) = self.read_cache.get(&chunk_key).await {
                return Ok(cached);
            }
            
            let is_fetching = {
                let fetching = self.fetching_chunks.lock().await;
                fetching.contains(&chunk_key)
            };
            
            if is_fetching {
                tokio::time::sleep(Duration::from_millis(10)).await;
            } else {
                break;
            }
        }
        
        {
            let mut fetching = self.fetching_chunks.lock().await;
            fetching.insert(chunk_key.clone());
        }

        let result = match self.fetch_range(path_clean, chunk_start, Self::READ_CHUNK_SIZE).await {
            Ok(data) => {
                let arc_data = Arc::new(data);
                self.read_cache.insert(chunk_key.clone(), arc_data.clone()).await;
                Ok(arc_data)
            },
            Err(e) => Err(e),
        };

        {
            let mut fetching = self.fetching_chunks.lock().await;
            fetching.remove(&chunk_key);
        }

        result
    }

    pub fn new(server_url: &str) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let url = Url::parse(server_url)?;
        
        let http_client = Client::builder()
            //.timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .tcp_keepalive(Duration::from_secs(60))
            .build()?;
        
        let runtime = global_runtime();
        let runtime_handle = runtime.handle().clone();
        let is_online = Arc::new(AtomicBool::new(true));
        let shutdown = Arc::new(AtomicBool::new(false));

        let metadata_cache = Cache::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(5)) // 5 secs
            .build();

        let read_cache = Cache::builder()
            .max_capacity(512) // 512 MB
            .time_to_idle(Duration::from_secs(10))
            .build();

        let health_runtime_handle = runtime_handle.clone();

        let fs = Arc::new(Self {
            server_url: url,
            http_client,
            runtime_handle,
            metadata_cache,
            read_cache,
            fetching_chunks: Arc::new(Mutex::new(std::collections::HashSet::new())),
            path_to_inode: RwLock::new(HashMap::from([(String::new(), 1)])),
            inode_to_path: RwLock::new(HashMap::from([(1, String::new())])),
            parent_map: RwLock::new(HashMap::from([(1, 1)])),
            next_inode: AtomicU64::new(2),
            is_online: is_online.clone(),
            shutdown: shutdown.clone(),
            health_task: Mutex::new(None),
        });
        
        let fs_check = fs.clone();
        let health_handle = health_runtime_handle.spawn(async move {
            let health_url = fs_check.server_url.join("health").unwrap();
            let mut was_online = true; 

            loop {
                if fs_check.shutdown.load(Ordering::SeqCst) {
                    break;
                }

                let res = fs_check.http_client.get(health_url.clone()).send().await;
                let now_online = res.is_ok() && res.unwrap().status().is_success();
                
                if !was_online && now_online {
                    fs_check.metadata_cache.invalidate_all();
                    fs_check.read_cache.invalidate_all();
                    
                    println!("[HEALTH] Server back online: caches cleared for consistency.");
                }

                if was_online && !now_online {
                    println!("\n[HEALTH] Server went OFFLINE! Requests will fail fast.");
                }
                
                fs_check.is_online.store(now_online, Ordering::SeqCst);
                
                was_online = now_online;

                let sleep_duration = if now_online { 5 } else { 2 };
                tokio::time::sleep(Duration::from_secs(sleep_duration)).await;
            }
        });

        fs.runtime_handle.block_on(async {
            let mut slot = fs.health_task.lock().await;
            *slot = Some(health_handle);
        });

        Ok(fs)
    }

    pub async fn shutdown_background_tasks(&self) {
        self.shutdown.store(true, Ordering::SeqCst);

        let mut slot = self.health_task.lock().await;
        if let Some(handle) = slot.take() {
            handle.abort();
            let _ = handle.await;
        }
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

            self.prefetch_next_chunk(path_clean.to_string(), chunk_start + Self::READ_CHUNK_SIZE as u64).await;
            
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

            if chunk.len() < Self::READ_CHUNK_SIZE {
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
        if ino == 1 {
            return Some(String::new());
        }

        self.inode_to_path.read().await.get(&ino).cloned()
    }

    async fn prefetch_next_chunk(&self, path_clean: String, next_chunk_start: u64) {
        if self.shutdown.load(Ordering::SeqCst) {
            return;
        }

        let chunk_key = format!("{}:{}", path_clean, next_chunk_start);
        
        if self.read_cache.contains_key(&chunk_key) {
            return;
        }

        {
            let mut fetching = self.fetching_chunks.lock().await;
            if fetching.contains(&chunk_key) {
                return;
            }
            fetching.insert(chunk_key.clone());
        }

        let self_clone = self.http_client.clone();
        let url_base = self.server_url.clone();
        let cache = self.read_cache.clone();
        let fetching_set = self.fetching_chunks.clone();
        
        self.runtime_handle.spawn(async move {
            let url_str = format!("files/{}?offset={}&size={}", path_clean, next_chunk_start, Self::READ_CHUNK_SIZE);
            if let Ok(url) = url_base.join(&url_str) {
                println!("[DEBUG] PREFETCH START -> {} | offset: {}", path_clean, next_chunk_start);
                if let Ok(resp) = self_clone.get(url).send().await {
                    if resp.status().is_success() {
                        if let Ok(bytes) = resp.bytes().await {
                            println!("[DEBUG] PREFETCH OK -> {} | offset: {}", path_clean, next_chunk_start);
                            cache.insert(chunk_key.clone(), Arc::new(bytes.to_vec())).await;
                        }
                    } else {
                        println!("[DEBUG] PREFETCH FAIL -> status {}", resp.status());
                    }
                }
            }
            
            let mut fetching = fetching_set.lock().await;
            fetching.remove(&chunk_key);
        });
    }
}

fn map_net_error(e: reqwest::Error) -> i32 {
    if e.is_timeout() { ETIMEDOUT }
    else if e.is_connect() { ECONNREFUSED }
    else { EIO }
}