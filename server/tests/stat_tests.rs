mod common;

use common::TestServer;
use reqwest::StatusCode;
use serde_json::Value;

#[test]
fn stat_root_and_file_work() {
    let server = TestServer::new();

    let write_status = server
        .client
        .put(server.url("/files/stat-file.txt"))
        .body("12345")
        .send()
        .expect("stat setup write request failed")
        .status();
    assert_eq!(write_status, StatusCode::OK);

    let root_stat = server
        .client
        .get(server.url("/stat"))
        .send()
        .expect("root stat request failed");
    assert_eq!(root_stat.status(), StatusCode::OK);
    let root_info: Value = root_stat.json().expect("invalid root stat JSON");
    assert_eq!(root_info["path"], "");
    assert_eq!(root_info["file_type"], "dir");

    let file_stat = server
        .client
        .get(server.url("/stat/stat-file.txt"))
        .send()
        .expect("file stat request failed");
    assert_eq!(file_stat.status(), StatusCode::OK);
    let file_info: Value = file_stat.json().expect("invalid file stat JSON");
    assert_eq!(file_info["file_type"], "file");
    assert_eq!(file_info["size"], 5);
}

#[test]
fn stat_error_cases_return_expected_statuses() {
    let server = TestServer::new();

    let missing_status = server
        .client
        .get(server.url("/stat/does-not-exist"))
        .send()
        .expect("missing stat request failed")
        .status();
    assert_eq!(missing_status, StatusCode::NOT_FOUND);

    let post_status = server
        .client
        .post(server.url("/stat"))
        .send()
        .expect("stat POST request failed")
        .status();
    assert_eq!(post_status, StatusCode::METHOD_NOT_ALLOWED);
}
