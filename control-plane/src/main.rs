use std::env;

use axum::serve;
use control_plane::{ControlPlane, ControlPlaneConfig};
use kube::Client;
use sqlx::postgres::PgPoolOptions;
use tracing::info;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "control_plane=info,tower_http=info".into()),
        )
        .init();

    let kube = Client::try_default()
        .await
        .expect("failed to create kubernetes client");

    let database_url = env::var("DATABASE_URL").expect("database url not set");
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .expect("could not connect to database");

    let config = ControlPlaneConfig::from_env();
    let control_plane = ControlPlane::new(kube, pool, config);
    control_plane.spawn_reconciler();
    let app = control_plane.router();

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8000")
        .await
        .expect("failed to bind listener");
    info!("control plane listening on 0.0.0.0:8000");
    serve(listener, app)
        .await
        .expect("control plane server exited unexpectedly");
}
