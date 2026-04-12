use std::net::IpAddr;
use std::path::PathBuf;
use std::{env, fs, net::SocketAddr};
use dotenvy::dotenv;
use actix_web::{App, HttpServer, web};
use actix_web::middleware::Logger;
use actix_cors::Cors;
use local_ip_address::local_ip;
use log::{info, warn};

mod apis;

fn storage_root() -> String {
    env::var("STORAGE_ROOT").unwrap_or_else(|_| "mnt/remote-fs".to_string())
}

fn get_local_ip_address() -> IpAddr {
    let my_local_ip = local_ip().unwrap_or_else(|_| {
        warn!("Impossibile trovare l'IP locale, uso 127.0.0.1");
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))
    });

    my_local_ip
}
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    
    let port = env::var("PORT")
        .unwrap_or_else(|_| "3000".to_string())
        .parse::<u16>()
        .expect("PORT must be a positive integer");

    let addr = SocketAddr::new(get_local_ip_address(), port);

    let root_dir = storage_root();
    let root_path = PathBuf::from(root_dir.clone());
    if !root_path.exists() {
        warn!("Storage directory '{}' does not exist, creating it...", root_path.display());
        fs::create_dir_all(&root_path)?;
    }

    let server = HttpServer::new(move || {
        let cors = Cors::default();
        
        App::new()
            .wrap(Logger::default())
            .app_data(web::Data::new(
                apis::AppState {
                    root_dir: root_dir.clone(),
                }
            ))
            .wrap(cors)
            .route("/", web::get().to(apis::index))
            .route("/list/{path:.*}", web::get().to(apis::list_directory))
            .route("/files/{path:.*}", web::get().to(apis::read_file))
            .route("/files/{path:.*}", web::put().to(apis::write_file))
            .route("/files/{path:.*}", web::patch().to(apis::patch_file))
            .route("/files/{path:.*}", web::head().to(apis::file_info))
            .route("/mkdir/{path:.*}", web::post().to(apis::create_directory))
            .route("/files/{path:.*}", web::delete().to(apis::delete_file))
            .route("/rename", web::post().to(apis::rename_entry))
            .route("/attrs/{path:.*}", web::patch().to(apis::set_attrs))
            .route("/health", web::get().to(apis::health))
    })
    .bind(&addr)?
    .run();

    let server_handle = server.handle();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("Shutdown on going...");
            server_handle.stop(true).await;
        }
    });

    info!("Server running on http://{}", addr);
    info!("Storage directory: {}", root_path.display());

    server.await
}