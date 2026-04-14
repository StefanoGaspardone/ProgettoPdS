mod common;

use actix_web::{http::StatusCode, test, App};
use serde_json::Value;

#[actix_web::test]
async fn index_endpoint_returns_service_metadata() {
    let (_tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let req = test::TestRequest::get().uri("/").to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;

    assert_eq!(body["name"], "Remote File System Server (Rust)");
    assert_eq!(body["version"], "1.0.0");
    assert!(body["endpoints"].is_array());
    assert!(body["endpoints"].as_array().unwrap_or(&Vec::new()).len() >= 5);
}

#[actix_web::test]
async fn health_endpoint_reports_success() {
    let (_tmp, state) = common::new_temp_state();
    let app = test::init_service(
        App::new()
            .app_data(state)
            .configure(common::configure_routes),
    )
    .await;

    let req = test::TestRequest::get().uri("/health").to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], true);
    assert_eq!(body["message"], "ok");
}
