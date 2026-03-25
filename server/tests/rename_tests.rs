mod common;

use common::TestServer;
use reqwest::StatusCode;
use serde_json::json;

#[test]
fn rename_success_cases_work() {
    let server = TestServer::new();

    let write_status = server
        .client
        .put(server.url("/files/rename-me.txt"))
        .body("rename-content")
        .send()
        .expect("rename setup write request failed")
        .status();
    assert_eq!(write_status, StatusCode::OK);

    let rename_status = server
        .client
        .post(server.url("/rename/rename-me.txt"))
        .json(&json!({ "new_path": "/renamed/final.txt" }))
        .send()
        .expect("rename request failed")
        .status();
    assert_eq!(rename_status, StatusCode::OK);

    let renamed_body = server
        .client
        .get(server.url("/files/renamed/final.txt"))
        .send()
        .expect("renamed file read request failed")
        .text()
        .expect("failed to decode renamed file body");
    assert_eq!(renamed_body, "rename-content");
}

#[test]
fn rename_error_cases_return_expected_statuses() {
    let server = TestServer::new();

    let invalid_json_status = server
        .client
        .post(server.url("/rename/x.txt"))
        .header("content-type", "application/json")
        .body("{invalid-json")
        .send()
        .expect("invalid JSON rename request failed")
        .status();
    assert_eq!(invalid_json_status, StatusCode::BAD_REQUEST);

    let missing_field_status = server
        .client
        .post(server.url("/rename/x.txt"))
        .json(&json!({ "unexpected": "value" }))
        .send()
        .expect("missing-field rename request failed")
        .status();
    assert_eq!(missing_field_status, StatusCode::UNPROCESSABLE_ENTITY);

    let missing_old_status = server
        .client
        .post(server.url("/rename/never-existed.txt"))
        .json(&json!({ "new_path": "whatever.txt" }))
        .send()
        .expect("missing-old rename request failed")
        .status();
    assert_eq!(missing_old_status, StatusCode::INTERNAL_SERVER_ERROR);

    let get_status = server
        .client
        .get(server.url("/rename/never-existed.txt"))
        .send()
        .expect("rename GET request failed")
        .status();
    assert_eq!(get_status, StatusCode::METHOD_NOT_ALLOWED);
}
