mod api;
mod deploy;
mod reconciler;
mod store;

use std::{env, sync::Arc};

use axum::serve;
use kube::Client;
use sqlx::postgres::PgPoolOptions;
use tracing::info;

use crate::store::Store;

#[derive(Clone)]
pub(crate) struct AppState {
    kube: Client,
    config: ControlPlaneConfig,
    store: Store,
}

#[derive(Clone)]
pub(crate) struct ControlPlaneConfig {
    base_domain: String,
    ingress_class_name: String,
    cluster_issuer: String,
}

impl ControlPlaneConfig {
    fn from_env() -> Self {
        Self {
            base_domain: env::var("BASE_DOMAIN")
                .unwrap_or_else(|_| "apps.joels.computer".to_string()),
            ingress_class_name: env::var("INGRESS_CLASS_NAME")
                .unwrap_or_else(|_| "traefik".to_string()),
            cluster_issuer: env::var("CLUSTER_ISSUER")
                .unwrap_or_else(|_| "letsencrypt-production".to_string()),
        }
    }
}

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

    let store = Store::new(pool);

    let config = ControlPlaneConfig::from_env();
    let state = Arc::new(AppState {
        kube,
        config,
        store,
    });

    tokio::spawn(reconciler::run(Arc::clone(&state)));
    let app = api::router(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8000")
        .await
        .expect("failed to bind listener");
    info!("control plane listening on 0.0.0.0:8000");
    serve(listener, app)
        .await
        .expect("control plane server exited unexpectedly");
}
