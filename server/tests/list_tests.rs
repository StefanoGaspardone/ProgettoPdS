mod common;

use common::TestServer;
use reqwest::StatusCode;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct FileInfo {
    name: String,
    file_type: String,
}

#[test]
fn list_root_and_nested_directory_work() {
    let server = TestServer::new();

    let mkdir_status = server
        .client
        .post(server.url("/mkdir/a/b"))
        .send()
        .expect("mkdir request failed")
        .status();
    assert_eq!(mkdir_status, StatusCode::CREATED);

    let write_status = server
        .client
        .put(server.url("/files/a/b/item.txt"))
        .body("data")
        .send()
        .expect("file write request failed")
        .status();
    assert_eq!(write_status, StatusCode::OK);

    let root_list: Vec<FileInfo> = server
        .client
        .get(server.url("/list"))
        .send()
        .expect("root list request failed")
        .json()
        .expect("invalid root list JSON");
    assert!(root_list.iter().any(|e| e.name == "a" && e.file_type == "dir"));

    let nested_list: Vec<FileInfo> = server
        .client
        .get(server.url("/list/a/b"))
        .send()
        .expect("nested list request failed")
        .json()
        .expect("invalid nested list JSON");
    assert!(nested_list.iter().any(|e| e.name == "item.txt" && e.file_type == "file"));
}

#[test]
fn list_missing_directory_returns_empty_list() {
    let server = TestServer::new();

    let missing_list: Vec<FileInfo> = server
        .client
        .get(server.url("/list/missing-dir"))
        .send()
        .expect("missing list request failed")
        .json()
        .expect("invalid missing list JSON");

    assert!(missing_list.is_empty());
}

#[test]
fn list_post_is_not_allowed() {
    let server = TestServer::new();

    let status = server
        .client
        .post(server.url("/list"))
        .send()
        .expect("list POST request failed")
        .status();

    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}
