mod cache;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod fuser;
#[cfg(target_os = "windows")]
mod dokany;

mod apis;

use anyhow::{Context, Result};
use crate::apis::ApiClient;
use dotenvy::dotenv;
use log::info;
use std::env;
#[cfg(target_os = "windows")]
use dokan::unmount;
#[cfg(target_os = "windows")]
use widestring::U16CString;

fn main() -> Result<()> {
    dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let rt = tokio::runtime::Runtime::new()
        .context("Failed to create Tokio runtime")?;
    let runtime_handle = rt.handle().clone();
    let _guard = rt.enter(); 

    let server_address = env::var("SERVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:3000".to_string());

    let default_mount = if cfg!(target_os = "windows") {
        "Z:\\".to_string()
    } else {
        "/tmp/remote-fs".to_string()
    };
    let mount_point = env::var("MOUNT_POINT").unwrap_or(default_mount);

    info!("Remote File System Client");
    info!("Server: {}", server_address);
    info!("Mount point: {}", mount_point);

    #[cfg(target_os = "windows")]
    {
        let mount_for_signal = mount_point.clone();
        ctrlc::set_handler(move || {
            info!("Dokan shutdown on going...");
            match U16CString::from_str(&mount_for_signal) {
                Ok(mp) => {
                    if !unmount(mp.as_ucstr()) {
                        log::warn!("Dokan unmount failed {}", mount_for_signal);
                    }
                }
                Err(_) => log::warn!("Invalid unmount mountpoint: {}", mount_for_signal),
            }
        })
        .context("Failed to install Ctrl+C handler")?;
    }

    let api_client = ApiClient::new(server_address, runtime_handle)
        .context("Failed to create API client")?;

    info!("Testing connection to server...");
    
    api_client
        .health_check()
        .map_err(|e| anyhow::anyhow!("{}", e))
        .context("Failed to connect to server")?;
    
    info!("Successfully connected to server");

    #[cfg(target_os = "windows")]
    {
        info!("Starting Dokany driver...");
        
        drop(_guard); 
        dokany::run_dokany_client(api_client, mount_point)?;
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        info!("Starting FUSE driver...");
        rt.block_on(async {
            fuser::run_fuser_client(api_client, mount_point).await
        })?;
    }

    Ok(())
}