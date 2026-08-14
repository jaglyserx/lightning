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
    let public_api = control_plane.router();
    let admin_api = control_plane.admin_router();

    let public_address = env::var("PUBLIC_API_ADDRESS").unwrap_or_else(|_| "0.0.0.0:8000".into());
    let admin_address = env::var("ADMIN_API_ADDRESS").unwrap_or_else(|_| "127.0.0.1:8001".into());
    let public_listener = tokio::net::TcpListener::bind(&public_address)
        .await
        .expect("failed to bind public api listener");
    let admin_listener = tokio::net::TcpListener::bind(&admin_address)
        .await
        .expect("failed to bind admin api listener");
    info!(%public_address, "control plane public api listening");
    info!(%admin_address, "control plane internal admin api listening");

    tokio::try_join!(
        serve(public_listener, public_api),
        serve(admin_listener, admin_api)
    )
    .expect("control plane server exited unexpectedly");
}
