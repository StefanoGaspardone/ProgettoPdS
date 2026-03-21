use dotenvy::dotenv;
use moka::future::Cache;
use std::{env, sync::{Arc, atomic::{AtomicU64, Ordering}}};
use std::time::{Duration, UNIX_EPOCH};
use tokio::runtime::Runtime;
use reqwest::{Url, Client};
use serde::{Serialize, Deserialize};

#[cfg(target_os = "windows")]
mod dokany;
#[cfg(target_os = "windows")]
use dokany::run_dokany_client;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod fuser;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use fuser::run_fuser_client;

type Inode = u64;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    pub file_type: String,
    pub size: u64,
    pub timestamp: u64,
    pub permissions: String,
}

pub struct RemoteFilesystem {
    pub server_url: Url,
    pub http_client: Client,
    pub runtime: Runtime,
    pub metadata_cache: Cache<String, (u64, FileInfo)>,
    pub inode_cache: Cache<u64, String>,
    pub next_inode: AtomicU64,
}

impl RemoteFilesystem {
    pub fn new(server_url: &str) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let url = Url::parse(server_url)?;
        let http_client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        let runtime = Runtime::new()?;

        let metadata_cache = Cache::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(10)) // 10 secs
            .build();

        let inode_cache = Cache::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(600)) // 600 secs
            .build();

        let fs = Arc::new(Self {
            server_url: url,
            http_client,
            runtime,
            metadata_cache,
            inode_cache,
            next_inode: AtomicU64::new(2), // Root è 1
        });

        let root_info = FileInfo {
            name: "".into(), path: "".into(), file_type: "dir".into(),
            size: 0, timestamp: 0, permissions: "755".into(),
        };
        
        fs.runtime.block_on(async {
            fs.metadata_cache.insert("".to_string(), (1, root_info)).await;
            fs.inode_cache.insert(1, "".to_string()).await;
        });

        Ok(fs)
    }

    pub fn generate_inode(&self) -> u64 {
        self.next_inode.fetch_add(1, Ordering::SeqCst)
    }

    pub fn get_next_inode(&self) -> u64 {
        self.next_inode.fetch_add(1, Ordering::SeqCst)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();

    let server_address = env::var("SERVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:3000".to_string());
    
    let default_mount = if cfg!(target_os = "windows") { "Z:" } else { "/tmp/remote-fs" };
    let mount_point = env::var("MOUNT_POINT").unwrap_or_else(|_| default_mount.to_string());

    println!("--- Client Remote FileSystem ---");
    println!("  > Server: {}", server_address);
    println!("  > Mount:  {}", mount_point);

    let fs = RemoteFilesystem::new(&server_address)?;

    #[cfg(target_os = "windows")]
    {
        println!("Driver: Dokany (Windows)");
        run_dokany_client(fs, mount_point);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        println!("Driver: FUSE (Linux/macOS)");
        let _ = create_dir_all(mount_point);
        run_fuser_client(fs, mount_point);
    }

    Ok(())
}