use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::Request,
    extract::{DefaultBodyLimit, Path, State, rejection::JsonRejection},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use tracing::warn;

use crate::{
    AppState,
    deploy::{
        AppSpec, AppStatusResponse, CreateAppRequest, DeployErrorKind, get_app_status_for_app,
    },
    store::{AppRecord, DeploymentRunRecord, NewApp},
};

pub(crate) fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .nest(
            "/v1",
            Router::new()
                .route("/apps", post(create_app))
                .route("/apps/{name}", get(fetch_app).delete(remove_app))
                .route("/apps/{name}/status", get(fetch_app_status)),
        )
        .fallback(route_not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn(request_timeout))
        .with_state(state)
}

#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: message.into(),
        }
    }

    fn service_unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "not_ready",
            message: message.into(),
        }
    }

    fn method_not_allowed() -> Self {
        Self {
            status: StatusCode::METHOD_NOT_ALLOWED,
            code: "method_not_allowed",
            message: "method not allowed".to_string(),
        }
    }

    fn request_timeout() -> Self {
        Self {
            status: StatusCode::REQUEST_TIMEOUT,
            code: "request_timeout",
            message: "request exceeded the 30 second limit".to_string(),
        }
    }

    fn from_json_rejection(rejection: JsonRejection) -> Self {
        let status = rejection.status();
        let code = if status == StatusCode::PAYLOAD_TOO_LARGE {
            "payload_too_large"
        } else {
            "invalid_json"
        };

        Self {
            status,
            code,
            message: rejection.body_text(),
        }
    }
}

#[derive(Serialize)]
struct ErrorResponse {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: ErrorDetail {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

#[derive(Serialize)]
struct AppResponse {
    app: AppRecord,
    deployment: Option<DeploymentRunRecord>,
}

#[derive(Serialize)]
struct AppStatusEnvelope {
    app: AppRecord,
    reconciliation: Option<DeploymentRunRecord>,
    deployment: Option<AppStatusResponse>,
}

async fn healthz() -> &'static str {
    "OK"
}

async fn readyz(State(state): State<Arc<AppState>>) -> Result<&'static str, ApiError> {
    let (database, kubernetes) = tokio::join!(state.store.ready(), state.kube.apiserver_version());

    let mut unavailable = Vec::new();
    if let Err(error) = database {
        warn!(error = %error, "readiness check could not reach postgres");
        unavailable.push("postgres");
    }
    if let Err(error) = kubernetes {
        warn!(error = %error, "readiness check could not reach kubernetes");
        unavailable.push("kubernetes");
    }

    if unavailable.is_empty() {
        Ok("OK")
    } else {
        Err(ApiError::service_unavailable(format!(
            "dependencies unavailable: {}",
            unavailable.join(", ")
        )))
    }
}

async fn route_not_found() -> ApiError {
    ApiError::not_found("route not found")
}

async fn method_not_allowed() -> ApiError {
    ApiError::method_not_allowed()
}

async fn request_timeout(request: Request, next: Next) -> Response {
    match tokio::time::timeout(Duration::from_secs(30), next.run(request)).await {
        Ok(response) => response,
        Err(_) => ApiError::request_timeout().into_response(),
    }
}

async fn create_app(
    State(state): State<Arc<AppState>>,
    request: Result<Json<CreateAppRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<AppResponse>), ApiError> {
    let Json(request) = request.map_err(ApiError::from_json_rejection)?;
    let app =
        AppSpec::from_request(request, &state.config.base_domain).map_err(ApiError::bad_request)?;
    let app = state
        .store
        .upsert_app(NewApp::from(app))
        .await
        .map_err(|err| ApiError::internal(err.to_string()))?;
    let deployment = state
        .store
        .enqueue_run(app.id)
        .await
        .map_err(|err| ApiError::internal(err.to_string()))?;

    Ok((
        StatusCode::ACCEPTED,
        Json(AppResponse {
            app,
            deployment: Some(deployment),
        }),
    ))
}

async fn fetch_app(
    Path(name): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AppResponse>, ApiError> {
    let app = find_app(&state, &name).await?;
    let deployment = state
        .store
        .latest_run(app.id)
        .await
        .map_err(|err| ApiError::internal(err.to_string()))?;

    Ok(Json(AppResponse { app, deployment }))
}

async fn fetch_app_status(
    Path(name): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AppStatusEnvelope>, ApiError> {
    let app = find_app(&state, &name).await?;
    let reconciliation = state
        .store
        .latest_run(app.id)
        .await
        .map_err(|err| ApiError::internal(err.to_string()))?;

    let deployment = if app.desired_state == "active" {
        match get_app_status_for_app(&state.kube, &app.namespace, &app.name, &app.hostname).await {
            Ok(status) => Some(status),
            Err(err) if matches!(err.kind, DeployErrorKind::NotFound) => None,
            Err(err) => return Err(ApiError::internal(err.message)),
        }
    } else {
        None
    };

    Ok(Json(AppStatusEnvelope {
        app,
        reconciliation,
        deployment,
    }))
}

async fn remove_app(
    Path(name): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<(StatusCode, Json<AppResponse>), ApiError> {
    let app = state
        .store
        .mark_app_deleted(&name)
        .await
        .map_err(|err| ApiError::internal(err.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("active app `{name}` was not found")))?;
    let deployment = state
        .store
        .enqueue_run(app.id)
        .await
        .map_err(|err| ApiError::internal(err.to_string()))?;

    Ok((
        StatusCode::ACCEPTED,
        Json(AppResponse {
            app,
            deployment: Some(deployment),
        }),
    ))
}

async fn find_app(state: &AppState, name: &str) -> Result<AppRecord, ApiError> {
    state
        .store
        .get_app_by_name(name)
        .await
        .map_err(|err| ApiError::internal(err.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("app `{name}` was not found")))
}

#[cfg(test)]
mod tests {
    use axum::{http::StatusCode, response::IntoResponse};
    use serde_json::json;

    use super::{ApiError, ErrorDetail, ErrorResponse};

    #[test]
    fn errors_use_the_versioned_api_envelope() {
        let value = serde_json::to_value(ErrorResponse {
            error: ErrorDetail {
                code: "bad_request",
                message: "name is required".to_string(),
            },
        })
        .expect("error response should serialize");

        assert_eq!(
            value,
            json!({
                "error": {
                    "code": "bad_request",
                    "message": "name is required"
                }
            })
        );
    }

    #[test]
    fn readiness_errors_return_service_unavailable() {
        let response = ApiError::service_unavailable("postgres unavailable").into_response();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
