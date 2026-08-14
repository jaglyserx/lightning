use serde::Serialize;
use sqlx::{
    FromRow, PgPool,
    types::{
        Uuid,
        chrono::{DateTime, Utc},
    },
};

use crate::deploy::AppSpec;

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct AppRecord {
    pub id: Uuid,
    pub name: String,
    pub namespace: String,
    pub image: String,
    pub port: i32,
    pub hostname: String,
    pub replicas: i32,
    pub desired_state: String,
    pub generation: i64,
    pub reconciled_generation: i64,
    pub last_reconciled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub(crate) struct NewApp {
    pub name: String,
    pub namespace: String,
    pub image: String,
    pub port: i32,
    pub hostname: String,
    pub replicas: i32,
}

impl From<AppSpec> for NewApp {
    fn from(app: AppSpec) -> Self {
        Self {
            name: app.name,
            namespace: app.namespace,
            image: app.image,
            port: i32::from(app.port),
            hostname: app.hostname,
            replicas: app.replicas,
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct DeploymentRunRecord {
    pub id: Uuid,
    pub app_id: Uuid,
    pub trigger_kind: String,
    pub status: String,
    pub status_message: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

const APP_COLUMNS: &str = r#"
    id, name, namespace, image, port, hostname, replicas, desired_state,
    generation, reconciled_generation, last_reconciled_at, created_at, updated_at
"#;

impl Store {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) async fn upsert_app(&self, app: NewApp) -> Result<AppRecord, sqlx::Error> {
        let sql = format!(
            r#"
            INSERT INTO apps (name, namespace, image, port, hostname, replicas, desired_state)
            VALUES ($1, $2, $3, $4, $5, $6, 'active')
            ON CONFLICT (name) DO UPDATE SET
                namespace = EXCLUDED.namespace,
                image = EXCLUDED.image,
                port = EXCLUDED.port,
                hostname = EXCLUDED.hostname,
                replicas = EXCLUDED.replicas,
                desired_state = 'active',
                generation = apps.generation + 1,
                updated_at = now()
            RETURNING {APP_COLUMNS}
            "#
        );

        sqlx::query_as::<_, AppRecord>(&sql)
            .bind(app.name)
            .bind(app.namespace)
            .bind(app.image)
            .bind(app.port)
            .bind(app.hostname)
            .bind(app.replicas)
            .fetch_one(&self.pool)
            .await
    }

    pub(crate) async fn get_app_by_name(
        &self,
        name: &str,
    ) -> Result<Option<AppRecord>, sqlx::Error> {
        let sql = format!("SELECT {APP_COLUMNS} FROM apps WHERE name = $1");
        sqlx::query_as::<_, AppRecord>(&sql)
            .bind(name)
            .fetch_optional(&self.pool)
            .await
    }

    pub(crate) async fn mark_app_deleted(
        &self,
        name: &str,
    ) -> Result<Option<AppRecord>, sqlx::Error> {
        let sql = format!(
            r#"
            UPDATE apps SET
                desired_state = 'deleted',
                generation = generation + 1,
                updated_at = now()
            WHERE name = $1 AND desired_state = 'active'
            RETURNING {APP_COLUMNS}
            "#
        );
        sqlx::query_as::<_, AppRecord>(&sql)
            .bind(name)
            .fetch_optional(&self.pool)
            .await
    }

    pub(crate) async fn apps_to_reconcile(&self) -> Result<Vec<AppRecord>, sqlx::Error> {
        let sql = format!(
            r#"
            SELECT {APP_COLUMNS}
            FROM apps
            WHERE reconciled_generation < generation
               OR (desired_state = 'active' AND last_reconciled_at < now() - interval '5 minutes')
            ORDER BY updated_at
            LIMIT 20
            "#
        );
        sqlx::query_as::<_, AppRecord>(&sql)
            .fetch_all(&self.pool)
            .await
    }

    pub(crate) async fn start_run(&self, app_id: Uuid) -> Result<DeploymentRunRecord, sqlx::Error> {
        sqlx::query_as::<_, DeploymentRunRecord>(
            r#"
            INSERT INTO deployment_runs (app_id, trigger_kind, status)
            VALUES ($1, 'reconciler', 'running')
            RETURNING id, app_id, trigger_kind, status, status_message, started_at, finished_at
            "#,
        )
        .bind(app_id)
        .fetch_one(&self.pool)
        .await
    }

    pub(crate) async fn enqueue_run(
        &self,
        app_id: Uuid,
    ) -> Result<DeploymentRunRecord, sqlx::Error> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            r#"
            UPDATE deployment_runs
            SET status = 'failed',
                status_message = 'superseded by newer desired state',
                finished_at = now()
            WHERE app_id = $1 AND status IN ('pending', 'running')
            "#,
        )
        .bind(app_id)
        .execute(&mut *transaction)
        .await?;

        let run = sqlx::query_as::<_, DeploymentRunRecord>(
            r#"
            INSERT INTO deployment_runs (app_id, trigger_kind, status)
            VALUES ($1, 'api', 'pending')
            RETURNING id, app_id, trigger_kind, status, status_message, started_at, finished_at
            "#,
        )
        .bind(app_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(run)
    }

    pub(crate) async fn mark_run_running(
        &self,
        run_id: Uuid,
    ) -> Result<DeploymentRunRecord, sqlx::Error> {
        sqlx::query_as::<_, DeploymentRunRecord>(
            r#"
            UPDATE deployment_runs
            SET status = 'running'
            WHERE id = $1 AND status = 'pending'
            RETURNING id, app_id, trigger_kind, status, status_message, started_at, finished_at
            "#,
        )
        .bind(run_id)
        .fetch_one(&self.pool)
        .await
    }

    pub(crate) async fn finish_run(
        &self,
        run_id: Uuid,
        app: &AppRecord,
        status: &str,
        message: &str,
    ) -> Result<(), sqlx::Error> {
        let mut transaction = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
            UPDATE deployment_runs
            SET status = $2, status_message = $3, finished_at = now()
            WHERE id = $1 AND status = 'running'
            "#,
        )
        .bind(run_id)
        .bind(status)
        .bind(message)
        .execute(&mut *transaction)
        .await?;

        if status == "succeeded" && result.rows_affected() == 1 {
            sqlx::query(
                r#"
                UPDATE apps
                SET reconciled_generation = GREATEST(reconciled_generation, $2),
                    last_reconciled_at = now()
                WHERE id = $1
                "#,
            )
            .bind(app.id)
            .bind(app.generation)
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await
    }

    pub(crate) async fn warn_run(&self, run_id: Uuid, message: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            UPDATE deployment_runs
            SET status_message = $2
            WHERE id = $1 AND status = 'running'
            "#,
        )
        .bind(run_id)
        .bind(message)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(crate) async fn latest_run(
        &self,
        app_id: Uuid,
    ) -> Result<Option<DeploymentRunRecord>, sqlx::Error> {
        sqlx::query_as::<_, DeploymentRunRecord>(
            r#"
            SELECT id, app_id, trigger_kind, status, status_message, started_at, finished_at
            FROM deployment_runs
            WHERE app_id = $1
            ORDER BY started_at DESC
            LIMIT 1
            "#,
        )
        .bind(app_id)
        .fetch_optional(&self.pool)
        .await
    }
}
