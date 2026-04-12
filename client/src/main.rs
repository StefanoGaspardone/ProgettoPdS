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
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::{self, Command};
#[cfg(target_os = "windows")]
use dokan::unmount;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
use widestring::U16CString;

const ARG_DAEMON: &str = "--daemon";
const ARG_FOREGROUND: &str = "--foreground";
const ARG_STOP: &str = "--stop";

fn pid_file_path() -> PathBuf {
    std::env::temp_dir().join("remotefs-client.pid")
}

fn write_pid_file(pid: u32) -> Result<()> {
    fs::write(pid_file_path(), pid.to_string()).context("Failed to write pid file")
}

fn read_pid_file() -> Result<u32> {
    let pid_raw = fs::read_to_string(pid_file_path()).context("Daemon pid file not found")?;
    let pid = pid_raw
        .trim()
        .parse::<u32>()
        .context("Invalid pid file content")?;
    Ok(pid)
}

fn remove_pid_file() {
    let _ = fs::remove_file(pid_file_path());
}

fn should_stop_daemon() -> bool {
    env::args().any(|a| a == ARG_STOP)
}

fn stop_daemon() -> Result<()> {
    let pid = read_pid_file()?;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let rc = unsafe { libc::kill(pid as i32, libc::SIGINT) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                remove_pid_file();
                println!("No running daemon found (stale pid file removed)");
                return Ok(());
            }
            return Err(anyhow::anyhow!("Failed to signal daemon pid {}: {}", pid, err));
        }
    }

    #[cfg(target_os = "windows")]
    {
        let status = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .context("Failed to execute taskkill")?;

        if !status.success() {
            return Err(anyhow::anyhow!("taskkill failed for pid {} with status {}", pid, status));
        }
    }

    remove_pid_file();
    println!("Client daemon stop requested for pid {}", pid);
    Ok(())
}

fn should_daemonize() -> bool {
    let has_daemon = env::args().any(|a| a == ARG_DAEMON);
    let is_foreground_child = env::args().any(|a| a == ARG_FOREGROUND);
    has_daemon && !is_foreground_child
}

fn spawn_daemon_child() -> Result<()> {
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
    write_pid_file(child.id())?;
    println!("Client daemon started with pid {}", child.id());
    Ok(())
}

fn main() -> Result<()> {
    if should_stop_daemon() {
        return stop_daemon();
    }

    if should_daemonize() {
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