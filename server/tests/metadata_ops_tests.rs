mod common;

use actix_web::{http::StatusCode, test, App};

#[actix_web::test]
async fn head_returns_content_length_and_last_modified() {
    let (_tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let put_req = test::TestRequest::put()
        .uri("/files/meta/file.txt")
        .set_payload("12345")
        .to_request();
    assert_eq!(test::call_service(&app, put_req).await.status(), StatusCode::OK);

    let head_req = test::TestRequest::default()
        .method(actix_web::http::Method::HEAD)
        .uri("/files/meta/file.txt")
        .to_request();
    let head_resp = test::call_service(&app, head_req).await;

    assert_eq!(head_resp.status(), StatusCode::OK);
    assert_eq!(
        head_resp
            .headers()
            .get("content-length")
            .and_then(|h| h.to_str().ok()),
        Some("5")
    );
    assert!(head_resp.headers().contains_key("last-modified"));
}

#[actix_web::test]
async fn rename_moves_file_and_creates_destination_parent() {
    let (_tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let put_req = test::TestRequest::put()
        .uri("/files/src/old.txt")
        .set_payload("hello")
        .to_request();
    assert_eq!(test::call_service(&app, put_req).await.status(), StatusCode::OK);

    let rename_req = test::TestRequest::post()
        .uri("/rename")
        .set_json(serde_json::json!({
            "from": "/src/old.txt",
            "to": "/dst/nested/new.txt"
        }))
        .to_request();
    let rename_resp = test::call_service(&app, rename_req).await;
    assert_eq!(rename_resp.status(), StatusCode::OK);

    let old_req = test::TestRequest::get().uri("/files/src/old.txt").to_request();
    let old_resp = test::call_service(&app, old_req).await;
    assert_eq!(old_resp.status(), StatusCode::NOT_FOUND);

    let new_req = test::TestRequest::get().uri("/files/dst/nested/new.txt").to_request();
    let new_resp = test::call_service(&app, new_req).await;
    assert_eq!(new_resp.status(), StatusCode::OK);
    let new_body = test::read_body(new_resp).await;
    assert_eq!(new_body.as_ref(), b"hello");
}

#[actix_web::test]
async fn rename_rejects_invalid_paths() {
    let (_tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let rename_req = test::TestRequest::post()
        .uri("/rename")
        .set_json(serde_json::json!({
            "from": "../escape.txt",
            "to": "/safe.txt"
        }))
        .to_request();
    let rename_resp = test::call_service(&app, rename_req).await;

    assert_eq!(rename_resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
async fn set_attrs_handles_not_found_and_success_case() {
    let (tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let missing_req = test::TestRequest::patch()
        .uri("/attrs/nope.txt")
        .set_json(serde_json::json!({ "mode": 420 }))
        .to_request();
    let missing_resp = test::call_service(&app, missing_req).await;
    assert_eq!(missing_resp.status(), StatusCode::NOT_FOUND);

    let put_req = test::TestRequest::put()
        .uri("/files/perm/file.txt")
        .set_payload("x")
        .to_request();
    assert_eq!(test::call_service(&app, put_req).await.status(), StatusCode::OK);

    let set_req = test::TestRequest::patch()
        .uri("/attrs/perm/file.txt")
        .set_json(serde_json::json!({ "mode": 384 }))
        .to_request();
    let set_resp = test::call_service(&app, set_req).await;
    assert_eq!(set_resp.status(), StatusCode::OK);

    let file_path = tmp.path().join("perm").join("file.txt");
    let metadata = std::fs::metadata(&file_path).expect("file metadata should be readable");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(not(unix))]
    {
        assert!(!metadata.permissions().readonly());
    }
}
