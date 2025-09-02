use std::time::UNIX_EPOCH;
use std::{collections::HashMap, time::SystemTime};
use std::sync::Mutex;
use tokio::runtime::Runtime;
use reqwest::Url;
use serde::{Serialize, Deserialize};

#[cfg(target_os = "windows")]
mod winfsp;
#[cfg(target_os = "windows")]
use winfsp::run_winfsp_client;

#[cfg(target_os = "linux")]
mod fuser;
#[cfg(target_os = "linux")]
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

pub struct RemoteFilesystem {
    pub server_url: Url,
    pub runtime: Runtime,
    pub metadata_cache: Mutex<HashMap<String, (u64, FileInfo)>>,
    pub inode_cache: Mutex<HashMap<u64, String>>,
    pub next_inode: u64,
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
            next_inode: FUSE_ROOT_ID + 1,
        }
    }
}

fn main() {
    let server_url = "http://localhost:3000";
    let filesystem = RemoteFilesystem::new(server_url);

    #[cfg(target_os = "windows")]
    run_winfsp_client(filesystem);

    #[cfg(target_os = "linux")]
    run_fuser_client(filesystem);
}