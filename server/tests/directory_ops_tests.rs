mod common;

use actix_web::{http::StatusCode, test, App};
use serde_json::Value;

#[actix_web::test]
async fn mkdir_list_delete_file_flow_works() {
    let (_tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let mkdir_req = test::TestRequest::post().uri("/mkdir/docs/sub").to_request();
    let mkdir_resp = test::call_service(&app, mkdir_req).await;
    assert_eq!(mkdir_resp.status(), StatusCode::OK);

    let put_req = test::TestRequest::put()
        .uri("/files/docs/sub/a.txt")
        .set_payload("abc")
        .to_request();
    let put_resp = test::call_service(&app, put_req).await;
    assert_eq!(put_resp.status(), StatusCode::OK);

    let list_req = test::TestRequest::get().uri("/list/docs/sub").to_request();
    let list_resp = test::call_service(&app, list_req).await;
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_body: Value = test::read_body_json(list_resp).await;

    let entries = list_body["entries"].as_array().expect("entries should be an array");
    let file_entry = entries
        .iter()
        .find(|entry| entry["name"] == "a.txt")
        .expect("a.txt should be listed");

    assert_eq!(file_entry["is_dir"], false);
    assert_eq!(file_entry["size"], 3);

    let delete_req = test::TestRequest::delete().uri("/files/docs/sub/a.txt").to_request();
    let delete_resp = test::call_service(&app, delete_req).await;
    assert_eq!(delete_resp.status(), StatusCode::OK);

    let delete_missing_req = test::TestRequest::delete().uri("/files/docs/sub/a.txt").to_request();
    let delete_missing_resp = test::call_service(&app, delete_missing_req).await;
    assert_eq!(delete_missing_resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
async fn list_rejects_path_traversal() {
    let (_tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let req = test::TestRequest::get().uri("/list/%2E%2E/evil").to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
async fn deleting_non_empty_directory_is_rejected() {
    let (_tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let mkdir_req = test::TestRequest::post().uri("/mkdir/data").to_request();
    assert_eq!(test::call_service(&app, mkdir_req).await.status(), StatusCode::OK);

    let put_req = test::TestRequest::put()
        .uri("/files/data/file.bin")
        .set_payload("x")
        .to_request();
    assert_eq!(test::call_service(&app, put_req).await.status(), StatusCode::OK);

    let delete_dir_req = test::TestRequest::delete().uri("/files/data").to_request();
    let delete_dir_resp = test::call_service(&app, delete_dir_req).await;

    assert!(!delete_dir_resp.status().is_success());
}
