use std::time::UNIX_EPOCH;
use std::{collections::HashMap, time::SystemTime};
use std::sync::Mutex;
use tokio::runtime::Runtime;
use reqwest::Url;
use serde::{Serialize, Deserialize};

#[cfg(target_os = "windows")]
mod dokany;
#[cfg(target_os = "windows")]
use dokany::run_dokany_client;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod fuser;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use fuser::run_fuser_client;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileInfo {
    name: String,
    path: String,
    #[serde(rename = "file_type")]
    file_type: String,
    size: usize,
    timestamp: u64,
    permissions: String,
}

// Implement Sync + Send for RemoteFilesystem to be safe for Dokan callbacks
pub struct RemoteFilesystem {
    pub server_url: Url,
    pub runtime: Runtime,
    pub metadata_cache: Mutex<HashMap<String, (u64, FileInfo)>>,
    pub inode_cache: Mutex<HashMap<u64, String>>,
    pub next_inode: Mutex<u64>,
}

impl RemoteFilesystem {
    pub fn new(server_url: &str) -> Self {
        const FUSE_ROOT_ID: u64 = 1;
        let mut metadata_cache = HashMap::new();
        let mut inode_cache = HashMap::new();

        let root_info = FileInfo {
            name: "".to_string(),
            path: "".to_string(),
            file_type: "dir".to_string(),
            size: 0,
            timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
            permissions: "755".to_string(),
        };
        metadata_cache.insert("".to_string(), (FUSE_ROOT_ID, root_info));
        inode_cache.insert(FUSE_ROOT_ID, "".to_string());

        Self {
            server_url: Url::parse(server_url).expect("Invalid server URL"),
            runtime: Runtime::new().expect("Failed to create Tokio runtime"),
            metadata_cache: Mutex::new(metadata_cache),
            inode_cache: Mutex::new(inode_cache),
            next_inode: Mutex::new(FUSE_ROOT_ID + 1),
        }
    }
}

fn main() {
    let server_url = "http://172.20.10.2:3000";
    let filesystem = RemoteFilesystem::new(server_url);

    #[cfg(target_os = "windows")]
    run_dokany_client(filesystem);

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    run_fuser_client(filesystem);
}