mod common;

use actix_web::{http::StatusCode, test, App};

#[actix_web::test]
async fn write_and_read_full_file_stream() {
    let (_tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let put_req = test::TestRequest::put()
        .uri("/files/docs/test.bin")
        .set_payload("abcdef")
        .to_request();
    let put_resp = test::call_service(&app, put_req).await;
    assert_eq!(put_resp.status(), StatusCode::OK);

    let get_req = test::TestRequest::get().uri("/files/docs/test.bin").to_request();
    let get_resp = test::call_service(&app, get_req).await;
    assert_eq!(get_resp.status(), StatusCode::OK);

    let body = test::read_body(get_resp).await;
    assert_eq!(body.as_ref(), b"abcdef");
}

#[actix_web::test]
async fn read_with_offset_and_size_query_returns_requested_slice() {
    let (_tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let put_req = test::TestRequest::put()
        .uri("/files/data/chunk.txt")
        .set_payload("0123456789")
        .to_request();
    assert_eq!(test::call_service(&app, put_req).await.status(), StatusCode::OK);

    let get_req = test::TestRequest::get()
        .uri("/files/data/chunk.txt?offset=3&size=4")
        .to_request();
    let get_resp = test::call_service(&app, get_req).await;

    assert_eq!(get_resp.status(), StatusCode::OK);
    let body = test::read_body(get_resp).await;
    assert_eq!(body.as_ref(), b"3456");
}

#[actix_web::test]
async fn read_with_range_header_returns_partial_content() {
    let (_tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let put_req = test::TestRequest::put()
        .uri("/files/range/file.txt")
        .set_payload("abcdef")
        .to_request();
    assert_eq!(test::call_service(&app, put_req).await.status(), StatusCode::OK);

    let get_req = test::TestRequest::get()
        .uri("/files/range/file.txt")
        .insert_header(("Range", "bytes=1-3"))
        .to_request();
    let get_resp = test::call_service(&app, get_req).await;

    assert_eq!(get_resp.status(), StatusCode::PARTIAL_CONTENT);
    let content_range = get_resp
        .headers()
        .get("Content-Range")
        .and_then(|value| value.to_str().ok())
        .expect("Content-Range header should be present");
    assert_eq!(content_range, "bytes 1-3/6");

    let body = test::read_body(get_resp).await;
    assert_eq!(body.as_ref(), b"bcd");
}

#[actix_web::test]
async fn read_with_offset_beyond_eof_returns_empty_body() {
    let (_tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let put_req = test::TestRequest::put()
        .uri("/files/eof.txt")
        .set_payload("xy")
        .to_request();
    assert_eq!(test::call_service(&app, put_req).await.status(), StatusCode::OK);

    let get_req = test::TestRequest::get()
        .uri("/files/eof.txt?offset=99&size=10")
        .to_request();
    let get_resp = test::call_service(&app, get_req).await;

    assert_eq!(get_resp.status(), StatusCode::OK);
    let body = test::read_body(get_resp).await;
    assert!(body.is_empty());
}

#[actix_web::test]
async fn patch_with_content_range_writes_partial_data_and_extends_file() {
    let (_tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let patch_req = test::TestRequest::patch()
        .uri("/files/patch/new.bin")
        .insert_header(("Content-Range", "bytes 2-4/*"))
        .set_payload("XYZ")
        .to_request();
    let patch_resp = test::call_service(&app, patch_req).await;

    assert_eq!(patch_resp.status(), StatusCode::OK);

    let get_req = test::TestRequest::get().uri("/files/patch/new.bin").to_request();
    let get_resp = test::call_service(&app, get_req).await;
    assert_eq!(get_resp.status(), StatusCode::OK);

    let body = test::read_body(get_resp).await;
    assert_eq!(body.as_ref(), &[0u8, 0u8, b'X', b'Y', b'Z']);
}

#[actix_web::test]
async fn patch_rejects_invalid_header_or_payload_length_mismatch() {
    let (_tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let missing_header_req = test::TestRequest::patch()
        .uri("/files/patch/missing.bin")
        .set_payload("abc")
        .to_request();
    let missing_header_resp = test::call_service(&app, missing_header_req).await;
    assert_eq!(missing_header_resp.status(), StatusCode::BAD_REQUEST);

    let mismatched_payload_req = test::TestRequest::patch()
        .uri("/files/patch/mismatch.bin")
        .insert_header(("Content-Range", "bytes 0-4/*"))
        .set_payload("abc")
        .to_request();
    let mismatched_payload_resp = test::call_service(&app, mismatched_payload_req).await;
    assert_eq!(mismatched_payload_resp.status(), StatusCode::BAD_REQUEST);
}
