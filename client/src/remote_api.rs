use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Clone)]
pub struct RemoteApi {
    client: Client,
    remote_url: String,
}

impl RemoteApi {
    pub fn new(remote_url: String) -> Self {
        Self {
            client: Client::new(),
            remote_url,
        }
    }

    fn build_url(&self, endpoint: &str, path: &Path) -> String {
        let path_str = path.to_str().unwrap_or("").replace('\\', "/");
        let encoded_path = urlencoding::encode(&path_str);
        format!("{}{}/{}", self.remote_url, endpoint, encoded_path)
    }

    pub fn list_directory(&self, path: &Path) -> Result<Vec<DirEntry>, reqwest::Error> {
        let url = self.build_url("/list", path);
        self.client.get(&url).send()?.json()
    }
    
    pub fn read_file(&self, path: &Path) -> Result<Vec<u8>, reqwest::Error> {
        let url = self.build_url("/files", path);
        Ok(self.client.get(&url).send()?.bytes()?.to_vec())
    }

    pub fn write_file(&self, path: &Path, data: Vec<u8>) -> Result<(), reqwest::Error> {
        let url = self.build_url("/files", path);
        self.client.put(&url).body(data).send()?.error_for_status()?;
        Ok(())
    }

    pub fn create_directory(&self, path: &Path) -> Result<(), reqwest::Error> {
        let url = self.build_url("/mkdir", path);
        self.client.post(&url).send()?.error_for_status()?;
        Ok(())
    }

    pub fn delete_path(&self, path: &Path) -> Result<(), reqwest::Error> {
        let url = self.build_url("/files", path);
        self.client.delete(&url).send()?.error_for_status()?;
        Ok(())
    }
}