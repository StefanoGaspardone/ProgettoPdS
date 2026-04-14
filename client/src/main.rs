mod cache;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod fuser;
#[cfg(target_os = "windows")]
mod dokany;

mod apis;
mod daemon;

use anyhow::{Context, Result};
use crate::apis::ApiClient;
use crate::daemon::{ARG_DAEMON, ARG_FOREGROUND};
use dotenvy::dotenv;
use log::info;
use std::env;
use std::process::Stdio;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::{self, Command};
#[cfg(target_os = "windows")]
use dokan::unmount;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
use widestring::U16CString;

fn stop_daemon() -> Result<()> {
    let pid = match daemon::running_daemon_pid()? {
        Some(pid) => pid,
        None => {
            println!("No process running.");
            return Ok(());
        }
    };

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let rc = unsafe { libc::kill(pid as i32, libc::SIGINT) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            
            if err.raw_os_error() == Some(libc::ESRCH) {
                daemon::remove_pid_file();

                println!("No process running.");
                return Ok(());
            }
            
            return Err(anyhow::anyhow!("Failed to signal daemon pid {}: {}", pid, err));
        }
    }

    #[cfg(target_os = "windows")]
    {
        let output = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output()
            .context("Failed to execute taskkill")?;

        if !output.status.success() {
            if !daemon::is_process_running(pid) {
                daemon::remove_pid_file();
                println!("No process running.");
                return Ok(());
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!(
                "taskkill failed for pid {} with status {}: {}",
                pid,
                output.status,
                stderr.trim()
            ));
        }
    }

    daemon::remove_pid_file();
    println!("Client daemon stop requested for pid {}", pid);
    Ok(())
}

fn spawn_daemon_child() -> Result<()> {
    if let Some(pid) = daemon::running_daemon_pid()? {
        println!("Daemon already running with pid {}", pid);
        return Ok(());
    }

    let current_exe = env::current_exe().context("Failed to resolve current executable")?;
    let args: Vec<String> = env::args().skip(1).filter(|a| a != ARG_DAEMON).collect();

    let mut cmd = std::process::Command::new(current_exe);
    cmd.args(args)
        .arg(ARG_FOREGROUND)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(target_os = "windows")]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }

                Ok(())
            });
        }
    }

    let child = cmd.spawn().context("Failed to spawn daemon child process")?;
    daemon::write_pid_file(child.id())?;
    
    println!("Client daemon started with pid {}", child.id());
    Ok(())
}

fn main() -> Result<()> {
    if daemon::should_stop_daemon() {
        return stop_daemon();
    }

    if daemon::should_daemonize() {
        return spawn_daemon_child();
    }

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

    for key in [
        "REMOTEFS_CHUNK_SIZE_KB",
        "REMOTEFS_CACHE_SIZE_MB",
        "REMOTEFS_FUSE_THREADS",
        "REMOTEFS_FUSE_CLONE_FD",
    ] {
        if let Ok(value) = env::var(key) {
            info!("{}={}", key, value);
        }
    }

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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mount_for_signal = mount_point.clone();
        ctrlc::set_handler(move || {
            info!("FUSE shutdown in progress...");

            #[cfg(target_os = "linux")]
            {
                match Command::new("fusermount")
                    .arg("-u")
                    .arg("-z")
                    .arg(&mount_for_signal)
                    .status()
                {
                    Ok(status) if status.success() => {
                        info!("FUSE unmounted successfully: {}", mount_for_signal);
                    }
                    Ok(status) => {
                        log::warn!("FUSE unmount returned non-zero status {status}: {}", mount_for_signal);
                    }
                    Err(e) => {
                        log::warn!("Failed to run fusermount for {}: {}", mount_for_signal, e);
                    }
                }
            }

            #[cfg(target_os = "macos")]
            {
                match Command::new("umount").arg(&mount_for_signal).status() {
                    Ok(status) if status.success() => {
                        info!("FUSE unmounted successfully: {}", mount_for_signal);
                    }
                    Ok(status) => {
                        log::warn!("umount returned non-zero status {status}: {}", mount_for_signal);
                    }
                    Err(e) => {
                        log::warn!("Failed to run umount for {}: {}", mount_for_signal, e);
                    }
                }
            }

            process::exit(0);
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