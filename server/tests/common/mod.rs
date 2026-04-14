#[path = "../../src/apis.rs"]
pub mod apis;

use actix_web::web;
use tempfile::TempDir;

pub fn new_temp_state() -> (TempDir, web::Data<apis::AppState>) {
    let tmp = tempfile::tempdir().expect("failed to create tempdir");
    let state = web::Data::new(apis::AppState {
        root_dir: tmp.path().to_string_lossy().to_string(),
    });
    (tmp, state)
}

pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/", web::get().to(apis::index))
        .route("/list/{path:.*}", web::get().to(apis::list_directory))
        .route("/files/{path:.*}", web::get().to(apis::read_file))
        .route("/files/{path:.*}", web::put().to(apis::write_file))
        .route("/files/{path:.*}", web::patch().to(apis::patch_file))
        .route("/files/{path:.*}", web::head().to(apis::file_info))
        .route("/mkdir/{path:.*}", web::post().to(apis::create_directory))
        .route("/files/{path:.*}", web::delete().to(apis::delete_file))
        .route("/rename", web::post().to(apis::rename_entry))
        .route("/attrs/{path:.*}", web::patch().to(apis::set_attrs))
        .route("/health", web::get().to(apis::health));
}
