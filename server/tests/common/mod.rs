use local_ip_address::local_ip;
use reqwest::blocking::Client;
use reqwest::StatusCode;
use std::fs;
use std::net::{IpAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

fn make_temp_storage_dir() -> TempDirGuard {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock went backwards")
        .as_nanos();

    let path = std::env::temp_dir().join(format!("server-test-storage-{}", unique));
    fs::create_dir_all(&path).expect("failed to create temp storage dir");

    TempDirGuard { path }
}

pub struct TestServer {
    host: IpAddr,
    port: u16,
    pub client: Client,
    _storage: TempDirGuard,
    _child: ChildGuard,
}

impl TestServer {
    pub fn new() -> Self {
        let host = local_ip().unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        let port = find_free_port(host);
        let storage = make_temp_storage_dir();

        let child = Command::new(env!("CARGO_BIN_EXE_server"))
            .env("PORT", port.to_string())
            .env("STORAGE_ROOT", storage.path.as_os_str())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to start server process");

        let child = ChildGuard { child };
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("failed to build HTTP client");

        let server = Self {
            host,
            port,
            client,
            _storage: storage,
            _child: child,
        };

        server.wait_until_ready();
        server
    }

    fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url(), path)
    }

    fn wait_until_ready(&self) {
        for _ in 0..40 {
            if let Ok(resp) = self.client.get(self.url("/health")).send() {
                if resp.status() == StatusCode::OK {
                    return;
                }
            }

            thread::sleep(Duration::from_millis(150));
        }

        panic!("server did not become ready in time");
    }
}
