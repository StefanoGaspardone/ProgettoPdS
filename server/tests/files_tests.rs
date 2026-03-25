mod common;

use common::TestServer;
use reqwest::StatusCode;

#[test]
fn files_put_get_and_range_read_work() {
    let server = TestServer::new();

    let write_status = server
        .client
        .put(server.url("/files/notes.txt"))
        .body("hello world")
        .send()
        .expect("put request failed")
        .status();
    assert_eq!(write_status, StatusCode::OK);

    let full_body = server
        .client
        .get(server.url("/files/notes.txt"))
        .send()
        .expect("full read request failed")
        .text()
        .expect("failed to decode full body");
    assert_eq!(full_body, "hello world");

    let range_body = server
        .client
        .get(server.url("/files/notes.txt?offset=6&size=5"))
        .send()
        .expect("range read request failed")
        .text()
        .expect("failed to decode range body");
    assert_eq!(range_body, "world");
}

#[test]
fn files_offset_write_and_edge_reads_work() {
    let server = TestServer::new();

    let create_status = server
        .client
        .put(server.url("/files/edge.txt"))
        .body("abcdef")
        .send()
        .expect("edge file create failed")
        .status();
    assert_eq!(create_status, StatusCode::OK);

    let patch_status = server
        .client
        .put(server.url("/files/edge.txt?offset=2"))
        .body("ZZ")
        .send()
        .expect("offset write request failed")
        .status();
    assert_eq!(patch_status, StatusCode::OK);

    let patched_body = server
        .client
        .get(server.url("/files/edge.txt"))
        .send()
        .expect("patched read request failed")
        .text()
        .expect("failed to decode patched body");
    assert_eq!(patched_body, "abZZef");

    let zero_size_body = server
        .client
        .get(server.url("/files/edge.txt?size=0"))
        .send()
        .expect("zero-size read request failed")
        .text()
        .expect("failed to decode zero-size body");
    assert_eq!(zero_size_body, "");

    let beyond_eof_body = server
        .client
        .get(server.url("/files/edge.txt?offset=1000"))
        .send()
        .expect("beyond EOF read request failed")
        .text()
        .expect("failed to decode beyond EOF body");
    assert_eq!(beyond_eof_body, "");
}

#[test]
fn files_delete_file_and_directory_work() {
    let server = TestServer::new();

    let write_status = server
        .client
        .put(server.url("/files/temp.txt"))
        .body("abc")
        .send()
        .expect("create file request failed")
        .status();
    assert_eq!(write_status, StatusCode::OK);

    let delete_file_status = server
        .client
        .delete(server.url("/files/temp.txt"))
        .send()
        .expect("delete file request failed")
        .status();
    assert_eq!(delete_file_status, StatusCode::NO_CONTENT);

    let missing_after_delete = server
        .client
        .get(server.url("/files/temp.txt"))
        .send()
        .expect("read after delete request failed")
        .status();
    assert_eq!(missing_after_delete, StatusCode::NOT_FOUND);

    let mkdir_status = server
        .client
        .post(server.url("/mkdir/tree/sub"))
        .send()
        .expect("mkdir tree request failed")
        .status();
    assert_eq!(mkdir_status, StatusCode::CREATED);

    let nested_write_status = server
        .client
        .put(server.url("/files/tree/sub/leaf.txt"))
        .body("leaf")
        .send()
        .expect("create nested file request failed")
        .status();
    assert_eq!(nested_write_status, StatusCode::OK);

    let delete_dir_status = server
        .client
        .delete(server.url("/files/tree"))
        .send()
        .expect("delete dir request failed")
        .status();
    assert_eq!(delete_dir_status, StatusCode::NO_CONTENT);
}

#[test]
fn files_error_cases_return_expected_statuses() {
    let server = TestServer::new();

    let bad_query_status = server
        .client
        .get(server.url("/files/edge.txt?offset=abc"))
        .send()
        .expect("bad query request failed")
        .status();
    assert_eq!(bad_query_status, StatusCode::BAD_REQUEST);

    let missing_read_status = server
        .client
        .get(server.url("/files/missing.txt"))
        .send()
        .expect("missing read request failed")
        .status();
    assert_eq!(missing_read_status, StatusCode::NOT_FOUND);

    let missing_delete_status = server
        .client
        .delete(server.url("/files/missing.txt"))
        .send()
        .expect("missing delete request failed")
        .status();
    assert_eq!(missing_delete_status, StatusCode::NOT_FOUND);

    let empty_delete_status = server
        .client
        .delete(server.url("/files/"))
        .send()
        .expect("empty delete request failed")
        .status();
    assert_eq!(empty_delete_status, StatusCode::BAD_REQUEST);

    let missing_parent_put_status = server
        .client
        .put(server.url("/files/missing/parent/file.txt"))
        .body("x")
        .send()
        .expect("missing parent put request failed")
        .status();
    assert_eq!(missing_parent_put_status, StatusCode::INTERNAL_SERVER_ERROR);

    let post_status = server
        .client
        .post(server.url("/files/temp.txt"))
        .send()
        .expect("files POST request failed")
        .status();
    assert_eq!(post_status, StatusCode::METHOD_NOT_ALLOWED);
}
