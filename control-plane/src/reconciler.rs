use std::{sync::Arc, time::Duration};

use tracing::{error, info, warn};

use crate::{
    AppState,
    deploy::{AppSpec, apply_app, delete_app_for_app, get_app_status_for_app},
    store::{AppRecord, DeploymentRunRecord},
};

enum ReconcileOutcome {
    Complete(String),
    Warning(String),
}

pub(crate) async fn run(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(10));

    loop {
        interval.tick().await;
        if let Err(err) = reconcile_once(&state).await {
            error!(error = %err, "reconciliation pass failed");
        }
    }
}

async fn reconcile_once(state: &AppState) -> Result<(), sqlx::Error> {
    for app in state.store.apps_to_reconcile().await? {
        if let Err(err) = reconcile_app(state, &app).await {
            error!(app = %app.name, error = %err, "could not record reconciliation result");
        }
    }
    Ok(())
}

async fn reconcile_app(state: &AppState, app: &AppRecord) -> Result<(), sqlx::Error> {
    let existing_run = state.store.latest_run(app.id).await?;
    let run = match existing_run.filter(|run| run.status == "running" || run.status == "pending") {
        Some(run) if run.status == "pending" => state.store.mark_run_running(run.id).await?,
        Some(run) => run,
        None => state.store.start_run(app.id).await?,
    };

    let result = if app.desired_state == "deleted" {
        reconcile_deleted_app(state, app, &run).await
    } else {
        reconcile_active_app(state, app, &run).await
    };

    match result {
        Ok(ReconcileOutcome::Complete(message)) => {
            state
                .store
                .finish_run(run.id, app, "succeeded", &message)
                .await?;
            info!(app = %app.name, generation = app.generation, "reconciliation succeeded");
        }
        Ok(ReconcileOutcome::Warning(message)) => {
            state.store.warn_run(run.id, &message).await?;
            warn!(app = %app.name, warning = %message, "reconciliation waiting for convergence");
        }
        Err(message) => {
            state
                .store
                .finish_run(run.id, app, "failed", &message)
                .await?;
            warn!(app = %app.name, error = %message, "reconciliation failed");
        }
    }

    Ok(())
}

async fn reconcile_deleted_app(
    state: &AppState,
    app: &AppRecord,
    run: &DeploymentRunRecord,
) -> Result<ReconcileOutcome, String> {
    let spec = AppSpec::try_from(app).map_err(|err| format!("invalid persisted app: {err}"))?;
    let status = delete_app_for_app(&state.kube, &spec)
        .await
        .map_err(|err| err.to_string())?;

    if status.status == "deleted" {
        return Ok(ReconcileOutcome::Complete(status.message));
    }

    if run_age_seconds(run) >= 120 {
        return Err("namespace deletion timed out".to_string());
    }

    Ok(ReconcileOutcome::Warning(
        "namespace deletion is still in progress".to_string(),
    ))
}

async fn reconcile_active_app(
    state: &AppState,
    app: &AppRecord,
    run: &DeploymentRunRecord,
) -> Result<ReconcileOutcome, String> {
    let spec = AppSpec::try_from(app).map_err(|err| format!("invalid persisted app: {err}"))?;
    apply_app(&state.kube, &state.config, &spec)
        .await
        .map_err(|err| err.to_string())?;

    let status = get_app_status_for_app(&state.kube, &app.namespace, &app.name, &app.hostname)
        .await
        .map_err(|err| err.to_string())?;

    if status.status == "ready" {
        return Ok(ReconcileOutcome::Complete(status.message));
    }

    if run_age_seconds(run) >= 120 {
        return Err(format!("rollout timed out: {}", status.message));
    }

    Ok(ReconcileOutcome::Warning(status.message))
}

fn run_age_seconds(run: &DeploymentRunRecord) -> i64 {
    (sqlx::types::chrono::Utc::now() - run.started_at).num_seconds()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::types::{Uuid, chrono::Utc};

    #[test]
    fn persisted_app_converts_to_spec() {
        let record = AppRecord {
            id: Uuid::nil(),
            name: "hello".into(),
            namespace: "hello".into(),
            image: "example/hello:v1".into(),
            port: 8080,
            hostname: "hello.apps.joels.computer".into(),
            replicas: 2,
            desired_state: "active".into(),
            generation: 1,
            reconciled_generation: 0,
            last_reconciled_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let spec = AppSpec::try_from(&record).expect("valid app");
        assert_eq!(spec.app_id, Some(record.id));
        assert_eq!(spec.port, 8080);
        assert_eq!(spec.replicas, 2);
    }
}
