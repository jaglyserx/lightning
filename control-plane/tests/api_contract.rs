use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header::CONTENT_TYPE},
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

fn admin_app() -> Router {
    let (kube_service, _handle) = mock::pair::<Request<KubeBody>, Response<KubeBody>>();
    let kube = Client::new(kube_service, "default");
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://lightning:lightning@localhost/lightning")
        .expect("test database URL should be valid");

    ControlPlane::new(kube, pool, ControlPlaneConfig::default()).admin_router()
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
async fn token_administration_is_not_exposed_on_the_public_router() {
    let response = app()
        .oneshot(Request::get("/v1/tokens").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn internal_api_validates_token_names_before_persistence() {
    let response = admin_app()
        .oneshot(
            Request::post("/v1/tokens")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"name":""}"#))
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
                "message": "token name must contain between 1 and 100 characters"
            }
        })
    );
}

#[tokio::test]
async fn protected_routes_require_a_bearer_token() {
    let response = app()
        .oneshot(
            Request::post("/v1/apps")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["www-authenticate"], "Bearer");
    assert_eq!(
        response_json(response).await,
        json!({
            "error": {
                "code": "unauthorized",
                "message": "a valid bearer token is required"
            }
        })
    );
}

#[tokio::test]
async fn protected_routes_reject_an_invalid_bearer_token() {
    let response = app()
        .oneshot(
            Request::post("/v1/apps")
                .header(CONTENT_TYPE, "application/json")
                .header("authorization", "Bearer invalid")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "unauthorized"
    );
}
