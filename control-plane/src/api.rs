use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;

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
        .route("/apps", post(create_app))
        .route("/apps/{name}", get(fetch_app).delete(remove_app))
        .route("/apps/{name}/status", get(fetch_app_status))
        .with_state(state)
}

#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: self.message,
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

async fn create_app(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateAppRequest>,
) -> Result<(StatusCode, Json<AppResponse>), ApiError> {
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
