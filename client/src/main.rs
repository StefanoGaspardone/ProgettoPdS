use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
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
struct FileInfo {
    name: String,
    path: String,
    #[serde(rename = "type")]
    file_type: String,
    size: usize,
    timestamp: String,
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
        Self {
            server_url: Url::parse(server_url).expect("Invalid server URL"), 
            runtime: Runtime::new().expect("Failed to create Tokio runtime"),  
            metadata_cache: Mutex::new(HashMap::new()),
            inode_cache: Mutex::new(HashMap::new()),
            next_inode: 2,
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