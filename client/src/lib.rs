use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use std::time::Duration;
use tokio::runtime::Runtime;
use reqwest::{Url, Client};
use serde::{Serialize, Deserialize};
use moka::future::Cache;
use libc::{ENOENT, EIO};

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
    pub inode_cache: Cache<u64, String>,
    next_inode: AtomicU64,
}

impl RemoteFilesystem {
    pub fn new(server_url: &str) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let url = Url::parse(server_url)?;
        let http_client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        let runtime = Runtime::new()?;

        let metadata_cache = Cache::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(5)) // 5 secs
            .build();

        let inode_cache = Cache::builder()
            .max_capacity(20_000)
            .time_to_live(Duration::from_secs(3600)) // 1 h
            .build();

        let fs = Arc::new(Self {
            server_url: url,
            http_client,
            runtime,
            metadata_cache,
            inode_cache,
            next_inode: AtomicU64::new(2),
        });

        fs.runtime.block_on(async {
            fs.metadata_cache.insert("".to_string(), (1, FileInfo::root())).await;
            fs.inode_cache.insert(1, "".to_string()).await;
        });

        Ok(fs)
    }

    // --- API methods ---

    pub async fn get_stat(&self, path: &str) -> Result<(u64, FileInfo), i32> {
        let path_clean = path.trim_start_matches('/');
        if let Some(cached) = self.metadata_cache.get(path_clean).await {
            return Ok(cached);
        }

        let url = self.server_url.join(&format!("stat/{}", path_clean)).map_err(|_| EIO)?;
        let resp = self.http_client.get(url).send().await.map_err(|_| EIO)?;
        if resp.status() == 404 { return Err(ENOENT); }
        
        let info: FileInfo = resp.json().await.map_err(|_| EIO)?;
        let ino = self.next_inode.fetch_add(1, Ordering::SeqCst);
        
        self.metadata_cache.insert(path_clean.to_string(), (ino, info.clone())).await;
        self.inode_cache.insert(ino, path_clean.to_string()).await;
        Ok((ino, info))
    }

    pub async fn list_dir(&self, path: &str) -> Result<Vec<(u64, FileInfo)>, i32> {
        let path_clean = path.trim_start_matches('/');
        let url = self.server_url.join(&format!("list/{}", path_clean)).map_err(|_| EIO)?;
        let resp = self.http_client.get(url).send().await.map_err(|_| EIO)?;
        let files: Vec<FileInfo> = resp.json().await.map_err(|_| EIO)?;

        let mut result = Vec::with_capacity(files.len());
        for file in files {
            let ino = if let Some((cached_ino, _)) = self.metadata_cache.get(&file.path).await {
                cached_ino
            } else {
                let new_ino = self.next_inode.fetch_add(1, Ordering::SeqCst);
                self.metadata_cache.insert(file.path.clone(), (new_ino, file.clone())).await;
                self.inode_cache.insert(new_ino, file.path.clone()).await;
                new_ino
            };
            
            result.push((ino, file));
        }
        Ok(result)
    }

    pub async fn read_file(&self, path: &str, offset: u64, size: u32) -> Result<Vec<u8>, i32> {
        let path_clean = path.trim_start_matches('/');
        let url_str = format!("files/{}?offset={}&size={}", path_clean, offset, size);
        let url = self.server_url.join(&url_str).map_err(|_| EIO)?;

        let resp = self.http_client.get(url)
            .send()
            .await
            .map_err(|_| EIO)?;

        if !resp.status().is_success() { return Err(EIO); }

        let data = resp.bytes().await.map_err(|_| EIO)?;
        Ok(data.to_vec())
    }

    pub async fn write_file(&self, path: &str, offset: u64, data: Vec<u8>) -> Result<(), i32> {
        let path_clean = path.trim_start_matches('/');
        let url_str = format!("files/{}?offset={}", path_clean, offset);
        let url = self.server_url.join(&url_str).map_err(|_| EIO)?;

        let resp = self.http_client.put(url)
            .body(data)
            .send()
            .await
            .map_err(|_| EIO)?;

        if resp.status().is_success() {
            self.metadata_cache.remove(path_clean).await;
            Ok(())
        } else {
            Err(EIO)
        }
    }

    pub async fn create_dir(&self, path: &str) -> Result<(), i32> {
        let path_clean = path.trim_start_matches('/');
        let url = self.server_url.join(&format!("mkdir/{}", path_clean)).map_err(|_| EIO)?;

        let resp = self.http_client.post(url).send().await.map_err(|_| EIO)?;
        if resp.status().is_success() { Ok(()) } else { Err(EIO) }
    }

    pub async fn delete_path(&self, path: &str) -> Result<(), i32> {
        let path_clean = path.trim_start_matches('/');
        let url = self.server_url.join(&format!("files/{}", path_clean)).map_err(|_| EIO)?;

        let resp = self.http_client.delete(url).send().await.map_err(|_| EIO)?;
        if resp.status().is_success() || resp.status() == 204 {
            self.metadata_cache.remove(path_clean).await;
            Ok(())
        } else {
            Err(EIO)
        }
    }

    pub async fn rename(&self, old_path: &str, new_path: &str) -> Result<(), i32> {
        let old_clean = old_path.trim_start_matches('/');
        let new_clean = new_path.trim_start_matches('/');
        let url = self.server_url.join(&format!("rename/{}", old_clean)).map_err(|_| EIO)?;

        let resp = self.http_client.post(url)
            .json(&RenameRequest { new_path: new_clean.to_string() })
            .send().await.map_err(|_| EIO)?;

        if resp.status().is_success() {
            if let Some((ino, info)) = self.metadata_cache.get(old_clean).await {
                let mut new_info = info.clone();
                new_info.path = new_clean.to_string();
                
                self.metadata_cache.remove(old_clean).await;
                self.metadata_cache.insert(new_clean.to_string(), (ino, new_info)).await;
                self.inode_cache.insert(ino, new_clean.to_string()).await;
            }
            Ok(())
        } else {
            Err(EIO)
        }
    }

    // --- UTILS ---

    pub async fn get_path_by_ino(&self, ino: u64) -> Option<String> {
        self.inode_cache.get(&ino).await
    }
}