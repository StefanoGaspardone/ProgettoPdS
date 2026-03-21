use axum::body::Body;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use axum::routing::{get, post};
use local_ip_address::local_ip;
use dotenvy::dotenv;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::net::TcpListener;
use tokio::signal;
use tokio_util::io::ReaderStream;
use std::env;
use std::io::SeekFrom;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use axum::extract::{Path, Query};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use futures_util::StreamExt;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    pub file_type: String,
    pub size: u64,
    pub timestamp: u64,
    pub permissions: String,
}

#[derive(Deserialize)]
pub struct ReadParams {
    pub offset: Option<u64>,
    pub size: Option<u64>,
}

#[derive(Deserialize)]
struct RenameRequest {
    new_path: String,
}

const STORAGE_ROOT: &str = "mnt/remote-fs";

fn get_local_ip_address() -> IpAddr {
    let my_local_ip = local_ip().unwrap_or_else(|_| {
        println!("Impossibile trovare l'IP locale, uso 127.0.0.1");
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))
    });

    my_local_ip
}

async fn list_dir(path: Option<Path<String>>) -> Json<Vec<FileInfo>> {
    let dir_path = path.map(|Path(p)| p).unwrap_or_default();
    let relative_path = dir_path.trim_start_matches('/');

    println!("\n[GET /list] /{}", relative_path);

    let full_path = PathBuf::from(STORAGE_ROOT).join(relative_path);
    let mut files = Vec::new();

    let mut entries = match fs::read_dir(&full_path).await {
        Ok(e) => e,
        Err(_err) => {
            return Json(files);
        }
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Ok(metadata) = entry.metadata().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            
            let entry_rel_path = if relative_path.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", relative_path, name)
            };

            files.push(FileInfo {
                name,
                path: entry_rel_path,
                file_type: if metadata.is_dir() { "dir".into() } else { "file".into() },
                size: metadata.len(),
                timestamp: metadata.modified()
                    .unwrap_or(std::time::SystemTime::now())
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                permissions: "755".to_string(),
            });
        }
    }

    Json(files)
}

async fn read_file(path: Option<Path<String>>, Query(params): Query<ReadParams>) -> impl IntoResponse {
    let file_path = path.map(|Path(p)| p).unwrap_or_default();
    let relative_path = file_path.trim_start_matches('/');

    let full_path = PathBuf::from(STORAGE_ROOT).join(relative_path);

    let mut file = match fs::File::open(&full_path).await {
        Ok(f) => f,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let offset = params.offset.unwrap_or(0);
    if offset > 0 {
        if let Err(_) = file.seek(SeekFrom::Start(offset)).await {
            return axum::http::StatusCode::BAD_REQUEST.into_response();
        }
    }

    println!("\n[GET /files] /{} with offset: {} and size: {:?}", relative_path, offset, params.size);

    if let Some(size) = params.size {
        let stream = ReaderStream::new(file.take(size));
        axum::body::Body::from_stream(stream).into_response()
    } else {
        let stream = ReaderStream::new(file);
        axum::body::Body::from_stream(stream).into_response()
    }
}

async fn get_stat(path: Option<Path<String>>) -> impl IntoResponse {
    let file_path = path.map(|Path(p)| p).unwrap_or_default();
    let relative_path = file_path.trim_start_matches('/');

    println!("\n[GET /stat] /{}", relative_path);

    let full_path = PathBuf::from(STORAGE_ROOT).join(relative_path);

    match fs::metadata(&full_path).await {
        Ok(metadata) => {
            let name = relative_path
                .split('/')
                .last()
                .unwrap_or("root")
                .to_string();

            let info = FileInfo {
                name,
                path: relative_path.to_string(),
                file_type: if metadata.is_dir() { "dir".to_string() } else { "file".to_string() },
                size: metadata.len(),
                timestamp: metadata.modified()
                    .unwrap_or(std::time::SystemTime::now())
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                permissions: "755".to_string(),
            };

            Json(info).into_response()
        }
        Err(_) => {
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

async fn write_file(path: Option<Path<String>>, Query(params): Query<ReadParams>, body: Body) -> impl IntoResponse {
    let file_path = path.map(|Path(p)| p).unwrap_or_default();
    let relative_path = file_path.trim_start_matches('/');

    println!("\n[PUT /files] /{} with body: {:?} and offset: {:?}", relative_path, body, params.offset);

    let full_path = PathBuf::from(STORAGE_ROOT).join(relative_path);

    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(params.offset.is_none())
        .open(&full_path)
        .await 
    {
        Ok(f) => f,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    if let Some(offset) = params.offset {
        if let Err(_) = file.seek(std::io::SeekFrom::Start(offset)).await {
            return StatusCode::BAD_REQUEST.into_response();
        }
    }

    let mut stream = body.into_data_stream();
    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                if let Err(e) = file.write_all(&chunk).await {
                    return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
                }
            }
            Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        }
    }

    let _ = file.flush().await;
    StatusCode::OK.into_response()
}

async fn create_dir(path: Option<Path<String>>) -> impl IntoResponse {
    let dir_path = path.map(|Path(p)| p).unwrap_or_default();
    
    if dir_path.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let relative_path = dir_path.trim_start_matches('/');

    println!("\n[POST /mkdir] /{}", relative_path);

    let full_path = PathBuf::from(STORAGE_ROOT).join(relative_path);

    match fs::create_dir_all(&full_path).await {
        Ok(_) => {
            StatusCode::CREATED.into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

async fn delete_file(path: Option<Path<String>>) -> impl IntoResponse {
    let file_path = path.map(|Path(p)| p).unwrap_or_default();
    
    if file_path.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let relative_path = file_path.trim_start_matches('/');

    println!("\n[DELETE /files] /{}", relative_path);

    let full_path = PathBuf::from(STORAGE_ROOT).join(relative_path);

    let metadata = match fs::metadata(&full_path).await {
        Ok(m) => m,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let result = if metadata.is_dir() {
        fs::remove_dir_all(&full_path).await
    } else {
        fs::remove_file(&full_path).await
    };

    match result {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

async fn rename_file(path: Path<String>, Json(payload): Json<RenameRequest>) -> impl IntoResponse {
    let old_relative = path.trim_start_matches('/');
    let new_relative = payload.new_path.trim_start_matches('/');

    println!("\n[DELETE /files] /{} to {}", old_relative, new_relative);

    let old_full = PathBuf::from(STORAGE_ROOT).join(old_relative);
    let new_full = PathBuf::from(STORAGE_ROOT).join(new_relative);

    if let Some(parent) = new_full.parent() {
        if !parent.exists() {
            return (
                StatusCode::NOT_FOUND,
                "Destination folder does not exist"
            ).into_response();
        }
    }

    match fs::rename(&old_full, &new_full).await {
        Ok(_) => {
            StatusCode::OK.into_response()
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[tokio::main]
async fn main() {
    dotenv().ok();
    let port = env::var("PORT")
        .unwrap_or_else(|_| "3000".to_string())
        .parse::<u16>()
        .expect("PORT must be a positive integer");

    let addr = SocketAddr::new(get_local_ip_address(), port);

    let root_path = PathBuf::from(STORAGE_ROOT);
    if!root_path.exists() {
        eprintln!("ERROR: storage directory '{}' does not exist, creating it...", root_path.display());
        fs::create_dir_all(&root_path).await.unwrap();
    }

    let app = Router::new()
        .route("/list", get(list_dir)) // GET /list
        .route("/list/{*path}", get(list_dir))

        .route("/files/", get(read_file).put(write_file).delete(delete_file)) // GET /files, PUT /files, DELETE /files
        .route("/files/{*path}", get(read_file).put(write_file).delete(delete_file))

        .route("/stat", get(get_stat)) // GET /stat
        .route("/stat/{*path}", get(get_stat))

        .route("/mkdir", post(create_dir)) // POST /mkdir
        .route("/mkdir/{*path}", post(create_dir))

        .route("/rename/{*path}", post(rename_file)); // POST /rename

    println!("SERVER LISTENING ON http://{}:{}", addr.ip(), addr.port());
    println!("STORAGE DIRECTORY: {}", STORAGE_ROOT);

    let listener = TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();

    println!("SERVER SHUT DOWN GRACEFULLY")
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(any(target_os = "windows"))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => { println!("\n[SIGINT] Shutdown starting..."); },
        _ = terminate => { println!("\n[SIGTERM] Shutdown starting..."); },
    }
}