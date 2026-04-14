use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
#[cfg(target_os = "windows")]
use std::process::Command;

pub const ARG_DAEMON: &str = "--daemon";
pub const ARG_FOREGROUND: &str = "--foreground";
pub const ARG_STOP: &str = "--stop";

pub fn pid_file_path() -> PathBuf {
    std::env::temp_dir().join("remotefs-client.pid")
}

pub fn write_pid_file_to(path: &Path, pid: u32) -> Result<()> {
    fs::write(path, pid.to_string()).context("Failed to write pid file")
}

pub fn read_pid_file_from(path: &Path) -> Result<u32> {
    let pid_raw = fs::read_to_string(path).context("Daemon pid file not found")?;
    let pid = pid_raw
        .trim()
        .parse::<u32>()
        .context("Invalid pid file content")?;
    
    Ok(pid)
}

pub fn remove_pid_file_at(path: &Path) {
    let _ = fs::remove_file(path);
}

pub fn write_pid_file(pid: u32) -> Result<()> {
    write_pid_file_to(&pid_file_path(), pid)
}

pub fn read_pid_file() -> Result<u32> {
    read_pid_file_from(&pid_file_path())
}

pub fn read_pid_file_optional_from(path: &Path) -> Result<Option<u32>> {
    match fs::read_to_string(path) {
        Ok(pid_raw) => {
            let pid = match pid_raw.trim().parse::<u32>() {
                Ok(pid) => pid,
                Err(_) => {
                    remove_pid_file_at(path);
                    return Ok(None);
                }
            };
            Ok(Some(pid))
        }
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).context("Failed to read pid file"),
    }
}

pub fn read_pid_file_optional() -> Result<Option<u32>> {
    read_pid_file_optional_from(&pid_file_path())
}

pub fn remove_pid_file() {
    remove_pid_file_at(&pid_file_path());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn is_process_running(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }

    let rc = unsafe { libc::kill(pid as i32, 0) };
    if rc == 0 {
        return true;
    }

    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(target_os = "windows")]
pub fn is_process_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    let output = match Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/FO", "CSV", "/NH"])
        .output()
    {
        Ok(output) => output,
        Err(_) => return false,
    };

    if !output.status.success() {
        return false;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pid_str = pid.to_string();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("INFO:") {
            continue;
        }

        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() > 1 {
            let listed_pid = cols[1].trim().trim_matches('"');
            if listed_pid == pid_str {
                return true;
            }
        }
    }

    false
}

pub fn running_daemon_pid_from(path: &Path) -> Result<Option<u32>> {
    let pid = match read_pid_file_optional_from(path)? {
        Some(pid) => pid,
        None => return Ok(None),
    };

    if is_process_running(pid) {
        return Ok(Some(pid));
    }

    remove_pid_file_at(path);
    Ok(None)
}

pub fn running_daemon_pid() -> Result<Option<u32>> {
    running_daemon_pid_from(&pid_file_path())
}

pub fn should_stop_daemon_from_args<I, S>(args: I) -> bool where I: IntoIterator<Item = S>, S: AsRef<str> {
    args.into_iter().any(|a| a.as_ref() == ARG_STOP)
}

pub fn should_stop_daemon() -> bool {
    should_stop_daemon_from_args(env::args())
}

pub fn should_daemonize_from_args<I, S>(args: I) -> bool where I: IntoIterator<Item = S>, S: AsRef<str> {
    let mut has_daemon = false;
    let mut is_foreground_child = false;

    for arg in args {
        let arg = arg.as_ref();
        
        if arg == ARG_DAEMON {
            has_daemon = true;
        } else if arg == ARG_FOREGROUND {
            is_foreground_child = true;
        }
    }

    has_daemon && !is_foreground_child
}

pub fn should_daemonize() -> bool {
    should_daemonize_from_args(env::args())
}
