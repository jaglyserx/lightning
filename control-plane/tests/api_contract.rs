use axum::{
    Router,
    body::Body,
    http::{Method, Request, Response, StatusCode, header::CONTENT_TYPE},
};
use control_plane::{ControlPlane, ControlPlaneConfig};
use http_body_util::BodyExt;
use kube::{Client, client::Body as KubeBody};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use tower_test::mock;

fn app() -> Router {
    let (kube_service, _handle) = mock::pair::<Request<KubeBody>, Response<KubeBody>>();
    let kube = Client::new(kube_service, "default");
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://lightning:lightning@localhost/lightning")
        .expect("test database URL should be valid");

    ControlPlane::new(kube, pool, ControlPlaneConfig::default()).router()
}

async fn response_json(response: Response<Body>) -> Value {
    let body = response
        .into_body()
        .collect()
        .await
        .expect("response body should be readable")
        .to_bytes();
    serde_json::from_slice(&body).expect("response should contain JSON")
}

#[tokio::test]
async fn healthz_reports_process_liveness() {
    let response = app()
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.as_ref(), b"OK");
}

#[tokio::test]
async fn unversioned_application_routes_are_not_exposed() {
    let response = app()
        .oneshot(Request::get("/apps").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response_json(response).await,
        json!({
            "error": {
                "code": "not_found",
                "message": "route not found"
            }
        })
    );
}

#[tokio::test]
async fn unsupported_methods_use_the_error_envelope() {
    let response = app()
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri("/v1/apps/example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        response_json(response).await,
        json!({
            "error": {
                "code": "method_not_allowed",
                "message": "method not allowed"
            }
        })
    );
}

#[tokio::test]
async fn malformed_json_uses_the_error_envelope() {
    let response = app()
        .oneshot(
            Request::post("/v1/apps")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "invalid_json"
    );
}

#[tokio::test]
async fn invalid_app_specs_are_rejected_before_persistence() {
    let response = app()
        .oneshot(
            Request::post("/v1/apps")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "",
                        "image": "example/web:v1",
                        "port": 8080
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(response).await,
        json!({
            "error": {
                "code": "bad_request",
                "message": "name is required"
            }
        })
    );
}

#[tokio::test]
async fn request_bodies_over_64_kib_are_rejected() {
    let response = app()
        .oneshot(
            Request::post("/v1/apps")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("x".repeat(64 * 1024 + 1)))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "payload_too_large"
    );
}
