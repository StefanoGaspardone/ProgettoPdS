use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::{Component, PathBuf},
    time::UNIX_EPOCH,
};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncSeekExt, AsyncWriteExt, SeekFrom},
};
use tokio_util::io::ReaderStream;

#[derive(Clone)]
struct AppState {
    remote_fs_root: PathBuf,
}

fn normalize_request_path(path_str: &str) -> Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in std::path::Path::new(path_str).components() {
        match component {
            Component::Normal(comp) => normalized.push(comp),
            Component::ParentDir => return Err("Invalid path (.. not allowed)".to_string()),
            _ => {} 
        }
    }
    Ok(normalized)
}

fn resolve_under_root(root: &std::path::Path, path_str: &str) -> Result<PathBuf, String> {
    let relative = normalize_request_path(path_str)?;
    let full = root.join(relative);
    if !full.starts_with(root) {
        return Err("Invalid path escape".to_string());
    }
    Ok(full)
}

#[derive(Serialize)]
struct FileStat {
    name: String,
    path: String,
    file_type: String,
    size: u64,
    timestamp: u64,
    permissions: String,
}

#[derive(Deserialize)]
struct RenameReq {
    from: String,
    to: String,
}

#[derive(Deserialize)]
struct TruncateReq {
    size: u64,
}

#[derive(Deserialize)]
struct FileQuery {
    offset: Option<u64>,
    size: Option<u64>,
}

#[derive(Serialize)]
struct PutRes {
    size: u64,
}

async fn handle_list(
    State(state): State<AppState>,
    path_opt: Option<Path<String>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let path_str = path_opt.map(|p| p.0).unwrap_or_default();
    let path = path_str.trim_start_matches('/');
    let full_path = resolve_under_root(&state.remote_fs_root, path)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let meta = fs::metadata(&full_path).await.map_err(|_| (StatusCode::NOT_FOUND, "Not found".to_string()))?;
    if !meta.is_dir() {
        return Err((StatusCode::BAD_REQUEST, "Not a directory".to_string()));
    }

    let mtime = meta.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap().as_secs();
    let etag = format!("W/\"{}-{:x}\"", meta.len(), mtime);
    
    if let Some(if_none_match) = headers.get(header::IF_NONE_MATCH) {
        if if_none_match.to_str().unwrap_or("") == etag {
            return Ok(StatusCode::NOT_MODIFIED.into_response());
        }
    }

    let mut entries = vec![];
    let mut dir = fs::read_dir(&full_path).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    
    while let Some(entry) = dir.next_entry().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))? {
        let entry_meta = entry.metadata().await.unwrap();
        let entry_mtime = entry_meta.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap().as_secs();
        
        let name = entry.file_name().to_string_lossy().to_string();
        
        let mut rel_path = path.to_string();
        if !rel_path.is_empty() {
            rel_path.push('/');
        }
        rel_path.push_str(&name);

        entries.push(FileStat {
            name,
            path: rel_path.replace('\\', "/"),
            file_type: if entry_meta.is_dir() { "dir".to_string() } else { "file".to_string() },
            size: entry_meta.len(),
            timestamp: entry_mtime,
            permissions: if entry_meta.is_dir() { "rwxr-xr-x".to_string() } else { "rw-r--r--".to_string() },
        });
    }

    let mut res = Json(entries).into_response();
    res.headers_mut().insert(header::ETAG, etag.parse().unwrap());
    res.headers_mut().insert(header::CACHE_CONTROL, "public, max-age=1, stale-while-revalidate=5".parse().unwrap());
    Ok(res)
}

async fn handle_stat(
    State(state): State<AppState>,
    path_opt: Option<Path<String>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let path_str = path_opt.map(|p| p.0).unwrap_or_default();
    let path = path_str.trim_start_matches('/');
    let full_path = resolve_under_root(&state.remote_fs_root, path)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let meta = fs::metadata(&full_path).await.map_err(|_| (StatusCode::NOT_FOUND, "Not found".to_string()))?;
    let mtime = meta.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap().as_secs();
    let etag = format!("W/\"{}-{:x}\"", meta.len(), mtime);

    if let Some(if_none_match) = headers.get(header::IF_NONE_MATCH) {
        if if_none_match.to_str().unwrap_or("") == etag {
            return Ok(StatusCode::NOT_MODIFIED.into_response());
        }
    }

    let stat = FileStat {
        name: full_path.file_name().unwrap_or_default().to_string_lossy().to_string(),
        path: path.to_string().replace('\\', "/"),
        file_type: if meta.is_dir() { "dir".to_string() } else { "file".to_string() },
        size: meta.len(),
        timestamp: mtime,
        permissions: if meta.is_dir() { "rwxr-xr-x".to_string() } else { "rw-r--r--".to_string() },
    };

    let mut res = Json(stat).into_response();
    res.headers_mut().insert(header::ETAG, etag.parse().unwrap());
    res.headers_mut().insert(header::CACHE_CONTROL, "public, max-age=1, stale-while-revalidate=5".parse().unwrap());
    Ok(res)
}

async fn handle_get_file(
    State(state): State<AppState>,
    path_opt: Option<Path<String>>,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let path_str = path_opt.map(|p| p.0).unwrap_or_default();
    let path = path_str.trim_start_matches('/');
    let full_path = resolve_under_root(&state.remote_fs_root, path)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let meta = fs::metadata(&full_path).await.map_err(|_| (StatusCode::NOT_FOUND, "Not found".to_string()))?;
    if meta.is_dir() {
        return Err((StatusCode::BAD_REQUEST, "Is a directory".to_string()));
    }

    let mtime = meta.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap().as_secs();
    let etag = format!("W/\"{}-{:x}\"", meta.len(), mtime);

    if let Some(if_none_match) = headers.get(header::IF_NONE_MATCH) {
        if if_none_match.to_str().unwrap_or("") == etag {
            return Ok(StatusCode::NOT_MODIFIED.into_response());
        }
    }

    let file_size = meta.len();
    let mut offset = query.offset.unwrap_or(0);
    let mut size = query.size.unwrap_or(file_size.saturating_sub(offset));

    if offset >= file_size {
        offset = 0;
        size = 0;
    } else if offset + size > file_size {
        size = file_size - offset;
    }

    let mut file = fs::File::open(&full_path).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    file.seek(SeekFrom::Start(offset)).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    
    let stream = ReaderStream::with_capacity(file.take(size), 64 * 1024);
    let body = Body::from_stream(stream);

    let res = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, size)
        .header(header::ETAG, etag)
        .body(body)
        .unwrap();

    Ok(res)
}

async fn handle_put_file(
    State(state): State<AppState>,
    path_opt: Option<Path<String>>,
    Query(query): Query<FileQuery>,
    req: Request,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let path_str = path_opt.map(|p| p.0).unwrap_or_default();
    let path = path_str.trim_start_matches('/');
    let full_path = resolve_under_root(&state.remote_fs_root, path)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    if let Ok(meta) = fs::metadata(&full_path).await {
        if meta.is_dir() {
            return Err((StatusCode::BAD_REQUEST, "Is a directory".to_string()));
        }
    }

    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    let offset = query.offset.unwrap_or(0);
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&full_path)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    file.seek(SeekFrom::Start(offset)).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut body_stream = req.into_body().into_data_stream();

    while let Some(chunk_result) = body_stream.next().await {
        let chunk = chunk_result.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        file.write_all(&chunk).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    
    file.flush().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let meta = fs::metadata(&full_path).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok((StatusCode::CREATED, Json(PutRes { size: meta.len() })))
}

async fn handle_delete(
    State(state): State<AppState>,
    path_opt: Option<Path<String>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let path_str = path_opt.map(|p| p.0).unwrap_or_default();
    let path = path_str.trim_start_matches('/');
    if path.is_empty() {
        return Err((StatusCode::CONFLICT, "Cannot remove root".to_string()));
    }

    let full_path = resolve_under_root(&state.remote_fs_root, path)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let meta = fs::metadata(&full_path).await.map_err(|_| (StatusCode::NOT_FOUND, "Not found".to_string()))?;
    if meta.is_dir() {
        fs::remove_dir_all(&full_path).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    } else {
        fs::remove_file(&full_path).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    Ok(StatusCode::OK)
}

async fn handle_mkdir(
    State(state): State<AppState>,
    path_opt: Option<Path<String>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let path_str = path_opt.map(|p| p.0).unwrap_or_default();
    let path = path_str.trim_start_matches('/');
    let full_path = resolve_under_root(&state.remote_fs_root, path)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    if fs::metadata(&full_path).await.is_ok() {
        return Err((StatusCode::CONFLICT, "Path already exists".to_string()));
    }

    fs::create_dir_all(&full_path).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::CREATED)
}

async fn handle_rename(
    State(state): State<AppState>,
    Json(payload): Json<RenameReq>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let from_path = resolve_under_root(&state.remote_fs_root, &payload.from)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let to_path = resolve_under_root(&state.remote_fs_root, &payload.to)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    if fs::metadata(&from_path).await.is_err() {
        return Err((StatusCode::NOT_FOUND, "Source not found".to_string()));
    }

    if let Some(parent) = to_path.parent() {
        fs::create_dir_all(parent).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    fs::rename(&from_path, &to_path).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

async fn handle_truncate(
    State(state): State<AppState>,
    path_opt: Option<Path<String>>,
    Json(payload): Json<TruncateReq>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let path_str = path_opt.map(|p| p.0).unwrap_or_default();
    let path = path_str.trim_start_matches('/');
    let full_path = resolve_under_root(&state.remote_fs_root, path)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let meta = fs::metadata(&full_path).await.map_err(|_| (StatusCode::NOT_FOUND, "Not found".to_string()))?;
    if meta.is_dir() {
        return Err((StatusCode::BAD_REQUEST, "Is a directory".to_string()));
    }

    let file = OpenOptions::new()
        .write(true)
        .open(&full_path)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        
    file.set_len(payload.size).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

#[tokio::main]
async fn main() {
    let current_dir = std::env::current_dir().expect("Failed to get current directory");
    let remote_fs_root = current_dir.join("mnt/remote-fs");
    
    fs::create_dir_all(&remote_fs_root).await.expect("Failed to create remote fs root");

    let state = AppState { remote_fs_root };

    let app = Router::new()
        .route("/list", get(handle_list))
        .route("/list/*path", get(handle_list))
        .route("/stat", get(handle_stat))
        .route("/stat/*path", get(handle_stat))
        .route("/files", get(handle_get_file).put(handle_put_file).delete(handle_delete))
        .route("/files/*path", get(handle_get_file).put(handle_put_file).delete(handle_delete))
        .route("/mkdir", post(handle_mkdir))
        .route("/mkdir/*path", post(handle_mkdir))
        .route("/truncate", post(handle_truncate))
        .route("/truncate/*path", post(handle_truncate))
        .route("/rename", post(handle_rename))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    println!("Rust Axum Server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}