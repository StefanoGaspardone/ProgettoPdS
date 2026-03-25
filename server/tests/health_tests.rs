mod common;

use common::TestServer;
use reqwest::StatusCode;

#[test]
fn health_get_returns_200() {
    let server = TestServer::new();

    let status = server
        .client
        .get(server.url("/health"))
        .send()
        .expect("health request failed")
        .status();

    assert_eq!(status, StatusCode::OK);
}

#[test]
fn health_post_is_not_allowed() {
    let server = TestServer::new();

    let status = server
        .client
        .post(server.url("/health"))
        .send()
        .expect("health POST request failed")
        .status();

    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}
