use dotenvy::dotenv;
use std::{env, process};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs::create_dir_all;

use client::RemoteFilesystem;

#[cfg(target_os = "windows")]
use client::dokany::run_dokany_client;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use client::fuser::run_fuser_client;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();

    let server_address = env::var("SERVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:3000".to_string());
    
    let default_mount = if cfg!(target_os = "windows") { "Z:" } else { "/tmp/remote-fs" };
    let mount_point = env::var("MOUNT_POINT").unwrap_or_else(|_| default_mount.to_string());
    
    println!("--- Client Remote FileSystem ---");
    println!("  > Server: {}", server_address);
    println!("  > Mount:  {}", mount_point);

    let mp_handler = mount_point.clone();

    ctrlc::set_handler(move || {
        println!("\n[SIGINT] Shutdown starting...");
        
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::process::Command;

            let _ = Command::new("fusermount")
                .arg("-u")
                .arg("-z")
                .arg(&mp_handler)
                .output();
        }

        println!("FILE SYSTEM SHUT DOWN GRACEFULLY");
        process::exit(0);
    }).expect("[SIGNINT] Handler failed to setup");

    let fs = RemoteFilesystem::new(&server_address)?;

    #[cfg(target_os = "windows")]
    {
        println!("Driver: Dokany (Windows)");
        run_dokany_client(fs, mount_point);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        println!("Driver: FUSE (Linux/macOS)");
        let _ = create_dir_all(&mount_point);
        run_fuser_client(fs, mount_point);
    }

    Ok(())
}