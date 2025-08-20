
use clap::Parser;

mod remote_api;

#[cfg(unix)]
mod fuse_impl;

#[cfg(windows)]
mod winfsp_impl;

/// A remote file system client in Rust
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Remote server URL (e.g., http://127.0.0.1:3000)
    #[arg(short, long)]
    remote_url: String,

    /// Local mount point
    #[arg(short, long)]
    mount_point: String,
}

fn main() {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    #[cfg(unix)]
    {
        fuse_impl::mount(&args.mount_point, args.remote_url);
    }

    #[cfg(windows)]
    {
        // Su Windows, i mount point sono lettere di unità (es. "X:") o directory
        winfsp_impl::mount(&args.mount_point, args.remote_url);
    }
}