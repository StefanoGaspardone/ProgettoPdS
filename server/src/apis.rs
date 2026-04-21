use actix_web::{web, HttpRequest, HttpResponse, Result};
use std::path::{Path, PathBuf, Component};
use std::fs;
use std::io::{ErrorKind, SeekFrom};
use serde::{Serialize, Deserialize};
use actix_web::http::header::{HeaderName, HeaderValue};
use actix_web::http::StatusCode;
use futures_util::StreamExt;
use tokio_util::io::ReaderStream;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use log::{info, warn};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct AppState {
    pub root_dir: String,
}

#[derive(Serialize)]
pub struct DirEntry {
    pub name: String,
    #[serde(rename = "is_dir")]
    pub is_dir: bool,
    pub size: u64,
    pub mtime: f64,
    pub ctime: f64,
    pub mode: u32,
}

#[derive(Serialize)]
pub struct ListResponse {
    pub entries: Vec<DirEntry>,
}

#[derive(Serialize)]
pub struct ApiResponse {
    pub success: bool,
    pub message: Option<String>,
    pub bytes_written: Option<usize>,
}

#[derive(Deserialize)]
pub struct RenameRequest {
    pub from: String,
    pub to: String,
}

#[derive(Deserialize)]
pub struct SetAttrsRequest {
    pub mode: Option<u32>,
}

#[derive(Deserialize)]
pub struct ReadQuery {
    pub offset: Option<u64>,
    pub size: Option<u64>,
}

fn fs_error_json(err: &std::io::Error, message: &'static str) -> HttpResponse {
    match err.kind() {
        ErrorKind::NotFound => HttpResponse::NotFound().json(message),
        ErrorKind::AlreadyExists => HttpResponse::Conflict().json(message),
        ErrorKind::PermissionDenied => HttpResponse::Forbidden().json(message),
        ErrorKind::DirectoryNotEmpty => HttpResponse::Conflict().json(message),
        _ => HttpResponse::InternalServerError().json(message),
    }
}

fn get_safe_path(root_dir: &str, request_path: &str) -> Option<PathBuf> {
    let trimmed = if request_path.starts_with('/') { &request_path[1..] } else { request_path };

    if trimmed.is_empty() {
        return Some(Path::new(root_dir).to_path_buf());
    }

    let mut rel = PathBuf::new();
    for comp in Path::new(trimmed).components() {
        match comp {
            Component::Normal(seg) => rel.push(seg),
            Component::CurDir => {},
            _ => return None,
        }
    }

    let candidate = Path::new(root_dir).join(rel);

    let base_abs = Path::new(root_dir).canonicalize().ok()?;
    let candidate_parent = candidate.parent().unwrap_or(Path::new(root_dir));
    let parent_abs = candidate_parent.canonicalize().unwrap_or(base_abs.clone());
    
    if parent_abs.starts_with(&base_abs) {
        Some(candidate)
    } else {
        None
    }
}

#[cfg(unix)]
fn permissions_mode(perms: &std::fs::Permissions) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    perms.mode()
}

#[cfg(not(unix))]
fn permissions_mode(perms: &std::fs::Permissions) -> u32 {
    if perms.readonly() { 0o555 } else { 0o666 }
}

#[cfg(unix)]
fn set_permissions_mode(perms: &mut std::fs::Permissions, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(mode);
}

#[cfg(not(unix))]
fn set_permissions_mode(perms: &mut std::fs::Permissions, mode: u32) {
    let writable = (mode & 0o222) != 0;
    perms.set_readonly(!writable);
}

pub async fn index() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(serde_json::json!({
        "name": "Remote File System Server (Rust)",
        "version": "1.0.0",
        "endpoints": [
            "GET /list/{path}",
            "GET /files/{path}",
            "PUT /files/{path}",
            "PATCH /files/{path}",
            "POST /mkdir/{path}",
            "DELETE /files/{path}",
            "HEAD /files/{path}"
        ]
    })))
}

pub async fn list_directory(
    req: HttpRequest,
    data: web::Data<AppState>
) -> Result<HttpResponse> {
    let path = req.match_info().query("path");
    let full_path = match get_safe_path(&data.root_dir, path) {
        Some(p) => p,
        None => return Ok(HttpResponse::BadRequest().json("Invalid path")),
    };

    if !full_path.exists() {
        return Ok(HttpResponse::NotFound().json("Directory not found"));
    }

    if !full_path.is_dir() {
        return Ok(HttpResponse::BadRequest().json("Not a directory"));
    }

    let mut entries = Vec::new();
    
    match fs::read_dir(&full_path) {
        Ok(entries_iter) => {
            for entry in entries_iter {
                if let Ok(entry) = entry {
                    match entry.metadata() {
                        Ok(metadata) => {
                            let is_dir = metadata.is_dir();
                            let size = metadata.len();
                            let mtime = system_time_to_secs(metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH));
                            let ctime = metadata.created().map(system_time_to_secs).unwrap_or(mtime);
                            let permissions = metadata.permissions();
                            let mode = permissions_mode(&permissions);

                            entries.push(DirEntry {
                                name: entry.file_name().to_string_lossy().to_string(),
                                is_dir,
                                size,
                                mtime,
                                ctime,
                                mode,
                            });
                        }
                        Err(_) => continue,
                    }
                }
            }
        }
        Err(_) => return Ok(HttpResponse::InternalServerError().json("Failed to read directory")),
    }

    Ok(HttpResponse::Ok().json(ListResponse { entries }))
}

pub async fn read_file(
    req: HttpRequest,
    data: web::Data<AppState>,
    query: web::Query<ReadQuery>,
) -> Result<HttpResponse> {
    let path = req.match_info().query("path");
    let full_path = match get_safe_path(&data.root_dir, path) {
        Some(p) => p,
        None => return Ok(HttpResponse::BadRequest().json("Invalid path")),
    };

    if !full_path.exists() {
        warn!("[GET /files] not found: {}", full_path.display());
        return Ok(HttpResponse::NotFound().json("File not found"));
    }

    if full_path.is_dir() {
        warn!("[GET /files] is directory: {}", full_path.display());
        return Ok(HttpResponse::BadRequest().json("Is a directory"));
    }

    if query.offset.is_some() || query.size.is_some() {
        let offset = query.offset.unwrap_or(0);
        let size = query.size.unwrap_or(128 * 1024);

        info!(
            "[GET /files] {} query offset={} size={}",
            full_path.display(),
            offset,
            size
        );

        let file_size = match tokio::fs::metadata(&full_path).await {
            Ok(m) => m.len(),
            Err(e) => return Ok(fs_error_json(&e, "Failed to stat file")),
        };

        if offset >= file_size {
            return Ok(HttpResponse::Ok()
                .content_type("application/octet-stream")
                .body(Vec::<u8>::new()));
        }

        let to_read = std::cmp::min(size, file_size - offset);
        return stream_file_slice(&full_path, offset, Some(to_read), StatusCode::OK, None).await;
    }

    if let Some(range_header) = req.headers().get("Range") {
        let range_str = range_header.to_str().unwrap_or("");
        info!("[GET /files] {} range={}", full_path.display(), range_str);
        
        if let Some((start, end)) = parse_range(range_str) {
            return handle_range_request(&full_path, start, end).await;
        }

        warn!("[GET /files] invalid range header: {}", range_str);
    }

    info!("[GET /files] {} full-file stream", full_path.display());

    match tokio::fs::File::open(&full_path).await {
        Ok(file) => {
            let stream = ReaderStream::with_capacity(file, 65536);
            
            Ok(HttpResponse::Ok()
                .content_type("application/octet-stream")
                .streaming(stream))
        }
        Err(_) => Ok(HttpResponse::InternalServerError().json("Failed to read file")),
    }
}

pub async fn write_file(
    req: HttpRequest,
    data: web::Data<AppState>,
    mut payload: web::Payload
) -> Result<HttpResponse> {
    let path = req.match_info().query("path");
    let full_path = match get_safe_path(&data.root_dir, path) {
        Some(p) => p,
        None => return Ok(HttpResponse::BadRequest().json("Invalid path")),
    };

    if let Some(parent) = full_path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return Ok(fs_error_json(&e, "Failed to create directories"));
        }
    }

    match tokio::fs::File::create(&full_path).await {
        Ok(mut file) => {
            let mut buf_writer = tokio::io::BufWriter::with_capacity(128 * 1024, file);
            let mut total: usize = 0;
            
            while let Some(chunk) = payload.next().await {
                let bytes = chunk.map_err(actix_web::error::ErrorBadRequest)?;
                buf_writer.write_all(&bytes).await.map_err(actix_web::error::ErrorInternalServerError)?;
                total += bytes.len();
            }
            
            buf_writer.flush().await.map_err(actix_web::error::ErrorInternalServerError)?;
            Ok(HttpResponse::Ok().json(ApiResponse { success: true, message: None, bytes_written: Some(total) }))
        }
        Err(e) => Ok(fs_error_json(&e, "Failed to write file")),
    }
}

pub async fn patch_file(
    req: HttpRequest,
    data: web::Data<AppState>,
    mut payload: web::Payload
) -> Result<HttpResponse> {
    let path = req.match_info().query("path");
    let full_path = match get_safe_path(&data.root_dir, path) {
        Some(p) => p,
        None => return Ok(HttpResponse::BadRequest().json("Invalid path")),
    };

    let range_header = match req.headers().get("Content-Range") {
        Some(h) => h.to_str().unwrap_or(""),
        None => return Ok(HttpResponse::BadRequest().json("Missing Content-Range header")),
    };

    if !range_header.starts_with("bytes ") {
        return Ok(HttpResponse::BadRequest().json("Invalid Content-Range format"));
    }

    let range_part = &range_header[6..]; // dopo 'bytes '
    let parts: Vec<&str> = range_part.split('/').collect();
    if parts.is_empty() {
        return Ok(HttpResponse::BadRequest().json("Invalid Content-Range parts"));
    }
    
    let span = parts[0]; // start-end
    let se: Vec<&str> = span.split('-').collect();
    if se.len() != 2 {
        return Ok(HttpResponse::BadRequest().json("Invalid start-end"));
    }
    
    let start = match se[0].parse::<u64>() { Ok(v) => v, Err(_) => return Ok(HttpResponse::BadRequest().json("Invalid start")) };
    let end = match se[1].parse::<u64>() { Ok(v) => v, Err(_) => return Ok(HttpResponse::BadRequest().json("Invalid end")) };
    if end < start {
        return Ok(HttpResponse::BadRequest().json("End < start"));
    }

    let expected_len = end - start + 1;

    if let Some(parent) = full_path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return Ok(fs_error_json(&e, "Failed to create parent directories"));
        }
    }

    let mut file = match tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .open(&full_path)
        .await
    {
        Ok(f) => f,
        Err(e) => return Ok(fs_error_json(&e, "Failed to open file")),
    };

    if let Err(e) = file.seek(SeekFrom::Start(start)).await {
        return Ok(fs_error_json(&e, "Failed to seek"));
    }

    let mut buf_writer = tokio::io::BufWriter::with_capacity(128 * 1024, file);
    let mut total: usize = 0;
    while let Some(chunk) = payload.next().await {
        let bytes = chunk.map_err(actix_web::error::ErrorBadRequest)?;
        if (total as u64) + (bytes.len() as u64) > expected_len {
            return Ok(HttpResponse::BadRequest().json("Payload larger than Content-Range"));
        }
        
        if let Err(e) = buf_writer.write_all(&bytes).await {
            return Ok(fs_error_json(&e, "Failed to write chunk"));
        }
        
        total += bytes.len();
    }

    if let Err(e) = buf_writer.flush().await {
        return Ok(fs_error_json(&e, "Failed to flush chunk"));
    }

    if (total as u64) != expected_len {
        return Ok(HttpResponse::BadRequest().json("Payload size does not match Content-Range"));
    }

    Ok(HttpResponse::Ok().json(ApiResponse { success: true, message: None, bytes_written: Some(total) }))
}

pub async fn file_info(
    req: HttpRequest,
    data: web::Data<AppState>
) -> Result<HttpResponse> {
    let path = req.match_info().query("path");
    let full_path = match get_safe_path(&data.root_dir, path) {
        Some(p) => p,
        None => return Ok(HttpResponse::BadRequest().finish()),
    };

    if !full_path.exists() {
        return Ok(HttpResponse::NotFound().finish());
    }

    if full_path.is_dir() {
        return Ok(HttpResponse::BadRequest().finish());
    }

    match fs::metadata(&full_path) {
        Ok(metadata) => {
            let mut response = HttpResponse::Ok().finish();
            
            response.headers_mut().insert(
                HeaderName::from_static("content-length"),
                HeaderValue::from_str(&metadata.len().to_string()).unwrap()
            );
            
            let modified = metadata.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let http_date = httpdate::fmt_http_date(modified);
            response.headers_mut().insert(
                HeaderName::from_static("last-modified"),
                HeaderValue::from_str(&http_date).unwrap()
            );
            
            Ok(response)
        }
        Err(_) => Ok(HttpResponse::InternalServerError().finish()),
    }
}

pub async fn create_directory(
    req: HttpRequest,
    data: web::Data<AppState>
) -> Result<HttpResponse> {
    let path = req.match_info().query("path");
    let full_path = match get_safe_path(&data.root_dir, path) {
        Some(p) => p,
        None => return Ok(HttpResponse::BadRequest().json("Invalid path")),
    };

    match tokio::fs::create_dir_all(&full_path).await {
        Ok(_) => Ok(HttpResponse::Ok().json(ApiResponse {
            success: true,
            message: None,
            bytes_written: None,
        })),
        Err(e) => Ok(fs_error_json(&e, "Failed to create directory")),
    }
}

pub async fn delete_file(
    req: HttpRequest,
    data: web::Data<AppState>
) -> Result<HttpResponse> {
    let path = req.match_info().query("path");
    let full_path = match get_safe_path(&data.root_dir, path) {
        Some(p) => p,
        None => return Ok(HttpResponse::BadRequest().json("Invalid path")),
    };

    if !full_path.exists() {
        return Ok(HttpResponse::NotFound().json("File not found"));
    }

    let result = if full_path.is_dir() {
        tokio::fs::remove_dir(&full_path).await
    } else {
        tokio::fs::remove_file(&full_path).await
    };

    match result {
        Ok(_) => Ok(HttpResponse::Ok().json(ApiResponse {
            success: true,
            message: None,
            bytes_written: None,
        })),
        Err(e) => Ok(fs_error_json(&e, "Failed to delete")),
    }
}

pub async fn rename_entry(
    data: web::Data<AppState>,
    payload: web::Json<RenameRequest>,
) -> Result<HttpResponse> {
    let from = match get_safe_path(&data.root_dir, &payload.from) {
        Some(p) => p,
        None => return Ok(HttpResponse::BadRequest().json("Invalid 'from' path")),
    };

    let to = match get_safe_path(&data.root_dir, &payload.to) {
        Some(p) => p,
        None => return Ok(HttpResponse::BadRequest().json("Invalid 'to' path")),
    };

    if !tokio::fs::try_exists(&from).await.unwrap_or(false) {
        return Ok(HttpResponse::NotFound().json("Source not found"));
    }

    if let Some(parent) = to.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return Ok(fs_error_json(&e, "Failed to prepare destination"));
        }
    }

    match tokio::fs::rename(&from, &to).await {
        Ok(_) => Ok(HttpResponse::Ok().json(ApiResponse { success: true, message: None, bytes_written: None })),
        Err(e) => Ok(fs_error_json(&e, "Failed to rename")),
    }
}

pub async fn health() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(ApiResponse {
        success: true,
        message: Some("ok".to_string()),
        bytes_written: None,
    }))
}

pub async fn set_attrs(
    req: HttpRequest,
    data: web::Data<AppState>,
    payload: web::Json<SetAttrsRequest>,
) -> Result<HttpResponse> {
    let path = req.match_info().query("path");
    let full_path = match get_safe_path(&data.root_dir, path) {
        Some(p) => p,
        None => return Ok(HttpResponse::BadRequest().json("Invalid path")),
    };

    if !full_path.exists() {
        return Ok(HttpResponse::NotFound().json("File not found"));
    }

    if let Some(mode) = payload.mode {
        match fs::metadata(&full_path) {
            Ok(metadata) => {
                let mut perms = metadata.permissions();
                set_permissions_mode(&mut perms, mode);
                
                if let Err(e) = fs::set_permissions(&full_path, perms) {
                    return Ok(fs_error_json(&e, "Failed to set permissions"));
                }
            }
            Err(e) => return Ok(fs_error_json(&e, "Failed to read metadata")),
        }
    }

    Ok(HttpResponse::Ok().json(ApiResponse {
        success: true,
        message: None,
        bytes_written: None,
    }))
}

fn parse_range(range_str: &str) -> Option<(u64, Option<u64>)> {
    let range_str = range_str.strip_prefix("bytes=")?;
    let parts: Vec<&str> = range_str.split('-').collect();
    
    if parts.len() != 2 {
        return None;
    }
    
    let start = parts[0].parse::<u64>().ok()?;
    let end = if parts[1].is_empty() {
        None
    } else {
        Some(parts[1].parse::<u64>().ok()?)
    };
    
    Some((start, end))
}

async fn handle_range_request(
    file_path: &Path,
    start: u64,
    end: Option<u64>
) -> Result<HttpResponse> {
    let file_size = tokio::fs::metadata(file_path)
        .await
        .map_err(actix_web::error::ErrorInternalServerError)?
        .len();
    
    let actual_end = end.unwrap_or(file_size - 1);
    let actual_end = actual_end.min(file_size - 1);
    
    if start > actual_end {
        warn!(
            "[GET /files] invalid range for {}: start={} end={}",
            file_path.display(),
            start,
            actual_end
        );
        return Ok(HttpResponse::RangeNotSatisfiable().finish());
    }

    info!(
        "[GET /files] {} bytes {}-{} / {}",
        file_path.display(),
        start,
        actual_end,
        file_size
    );
    
    let content_range = format!("bytes {}-{}/{}", start, actual_end, file_size);
    stream_file_slice(
        file_path,
        start,
        Some(actual_end - start + 1),
        StatusCode::PARTIAL_CONTENT,
        Some(content_range),
    )
    .await
}

async fn stream_file_slice(
    file_path: &Path,
    start: u64,
    len: Option<u64>,
    status: StatusCode,
    content_range: Option<String>,
) -> Result<HttpResponse> {
    let mut file = tokio::fs::File::open(file_path)
        .await
        .map_err(actix_web::error::ErrorInternalServerError)?;

    file.seek(SeekFrom::Start(start))
        .await
        .map_err(actix_web::error::ErrorInternalServerError)?;

    let mut builder = HttpResponse::build(status);
    builder.content_type("application/octet-stream");

    if let Some(ref cr) = content_range {
        builder.insert_header(("Content-Range", cr.clone()));
    }

    let response = if let Some(max_len) = len {
        let stream = ReaderStream::with_capacity(file.take(max_len), 65536);
        builder.streaming(stream)
    } else {
        let stream = ReaderStream::with_capacity(file, 65536);
        builder.streaming(stream)
    };

    Ok(response)
}

fn system_time_to_secs(time: SystemTime) -> f64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}