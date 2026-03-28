use dotenvy::dotenv;
use std::{env, process, thread, time::Duration};

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

    let fs = loop {
        match RemoteFilesystem::new(&server_address) {
            Ok(fs_instance) => {
                let is_alive = fs_instance.runtime_handle.block_on(async {
                    let health_url = fs_instance.server_url.join("health").ok()?;
                    
                    fs_instance.http_client.get(health_url)
                        .timeout(Duration::from_secs(2))
                        .send()
                        .await
                        .ok()?
                        .status()
                        .is_success()
                        .then(|| ())
                });

                if is_alive.is_some() {
                    println!("  > [OK] Server online and reached");
                    break fs_instance;
                }
            }
            Err(e) => {
                println!("  > [WARN] Init error: {}", e);
            }
        }
        println!("  > [RETRY] Server unavailable, retrying in 5 seconds...");
        thread::sleep(Duration::from_secs(5));
    };

    let fs_for_signal = fs.clone();

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mount_point_for_signal = mount_point.clone();

    ctrlc::set_handler(move || {
        println!("\n[SIGINT] Shutdown starting...");

        fs_for_signal.runtime_handle.block_on(async {
            fs_for_signal.shutdown_background_tasks().await;
        });
        
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::process::Command;

            let _ = Command::new("fusermount")
                .arg("-u")
                .arg("-z")
                .arg(&mount_point_for_signal)
                .output();
        }

        println!("FILE SYSTEM SHUT DOWN GRACEFULLY");
        process::exit(0);
    }).expect("[SIGNINT] Handler failed to setup");

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