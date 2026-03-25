use reqwest::blocking::Client;
use reqwest::StatusCode;
use std::fs;
use std::net::{IpAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::sync::Arc;
#[cfg(target_os = "windows")]
use std::sync::{Mutex, MutexGuard, OnceLock};
#[cfg(target_os = "windows")]
use widestring::U16CString;
use local_ip_address::local_ip;
use client::RemoteFilesystem; 

#[cfg(any(target_os = "linux", target_os = "macos"))]
use client::fuser::run_fuser_client;
#[cfg(target_os = "windows")]
use client::dokany::run_dokany_client;

#[cfg(target_os = "windows")]
fn dokan_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct ChildGuard {
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct TempDirGuard {
    path: PathBuf,
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn find_free_port(host: IpAddr) -> u16 {
    let listener = TcpListener::bind((host, 0)).expect("failed to find a free port");
    listener.local_addr().expect("failed to read local addr").port()
}

fn make_temp_dir(prefix: &str) -> TempDirGuard {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock went backwards")
        .as_nanos();

    let path = std::env::temp_dir().join(format!("{}-{}", prefix, unique));
    fs::create_dir_all(&path).expect("failed to create temp dir");

    TempDirGuard { path }
}

pub struct TestEnvironment {
    pub mount_point: PathBuf,
    _server_storage: TempDirGuard,
    _mount_dir: TempDirGuard,
    _server_child: ChildGuard,
    fs_instance: Arc<RemoteFilesystem>,
    #[cfg(target_os = "windows")]
    _dokan_guard: MutexGuard<'static, ()>,
}

impl TestEnvironment {
    pub fn new() -> Self {
        #[cfg(target_os = "windows")]
        let dokan_guard = dokan_test_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let host = local_ip().unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        let port = find_free_port(host);
        let server_storage = make_temp_dir("server-storage");
        let mount_dir = make_temp_dir("client-mount");
        let server_url = format!("http://{}:{}", host, port);

        let child = Command::new("cargo")
            .args(["run", "--manifest-path", "../server/Cargo.toml"])
            .env("PORT", port.to_string())
            .env("STORAGE_ROOT", server_storage.path.as_os_str())
            .stdout(Stdio::null()) // Ripristinato!
            .stderr(Stdio::null()) // Ripristinato!
            .spawn()
            .expect("failed to start server process");

        let server_child = ChildGuard { child };

        let http_client = Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
        let mut ready = false;
        for _ in 0..40 {
            if let Ok(resp) = http_client.get(format!("{}/health", server_url)).send() {
                if resp.status() == StatusCode::OK {
                    ready = true;
                    break;
                }
            }
            thread::sleep(Duration::from_millis(150));
        }
        if !ready {
            panic!("Server did not become ready in time");
        }

        let fs_instance = RemoteFilesystem::new(&server_url).expect("Failed to init RemoteFilesystem");
        let fs_for_thread = fs_instance.clone();
        let mount_path_str = mount_dir.path.to_string_lossy().to_string();

        thread::spawn(move || {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            run_fuser_client(fs_for_thread, mount_path_str);
            
            #[cfg(target_os = "windows")]
            run_dokany_client(fs_for_thread, mount_path_str);
        });

        thread::sleep(Duration::from_millis(500));

        Self {
            mount_point: mount_dir.path.clone(),
            _server_storage: server_storage,
            _mount_dir: mount_dir,
            _server_child: server_child,
            fs_instance,
            #[cfg(target_os = "windows")]
            _dokan_guard: dokan_guard,
        }
    }
}

impl Drop for TestEnvironment {
    fn drop(&mut self) {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let _ = Command::new("fusermount")
                .arg("-u")
                .arg("-z")
                .arg(&self.mount_point)
                .output();
        }

        #[cfg(target_os = "windows")]
        {
            if let Ok(mp) = U16CString::from_os_str(&self.mount_point) {
                let _ = dokan::unmount(&mp);
            }
        }

        self.fs_instance.runtime_handle.block_on(async {
            self.fs_instance.shutdown_background_tasks().await;
        });
        
        thread::sleep(Duration::from_millis(200));
    }
}