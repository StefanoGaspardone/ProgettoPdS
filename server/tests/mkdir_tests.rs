mod common;

use common::TestServer;
use reqwest::StatusCode;

#[test]
fn mkdir_create_and_idempotency_work() {
    let server = TestServer::new();

    let create_status = server
        .client
        .post(server.url("/mkdir/same/dir"))
        .send()
        .expect("mkdir create request failed")
        .status();
    assert_eq!(create_status, StatusCode::CREATED);

    let create_again_status = server
        .client
        .post(server.url("/mkdir/same/dir"))
        .send()
        .expect("mkdir create-again request failed")
        .status();
    assert_eq!(create_again_status, StatusCode::CREATED);
}

#[test]
fn mkdir_error_cases_return_expected_statuses() {
    let server = TestServer::new();

    let empty_status = server
        .client
        .post(server.url("/mkdir"))
        .send()
        .expect("empty mkdir request failed")
        .status();
    assert_eq!(empty_status, StatusCode::BAD_REQUEST);

    let get_status = server
        .client
        .get(server.url("/mkdir/test"))
        .send()
        .expect("mkdir GET request failed")
        .status();
    assert_eq!(get_status, StatusCode::METHOD_NOT_ALLOWED);
}
