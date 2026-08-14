mod api;
pub mod auth;
mod deploy;
mod reconciler;
mod store;

use std::{env, sync::Arc};

use axum::Router;
use kube::Client;
use sqlx::PgPool;
use tokio::task::JoinHandle;

use crate::store::Store;

#[derive(Clone)]
pub(crate) struct AppState {
    kube: Client,
    config: ControlPlaneConfig,
    store: Store,
}

#[derive(Clone)]
pub struct ControlPlaneConfig {
    pub(crate) base_domain: String,
    pub(crate) ingress_class_name: String,
    pub(crate) cluster_issuer: String,
}

impl Default for ControlPlaneConfig {
    fn default() -> Self {
        Self {
            base_domain: "apps.joels.computer".to_string(),
            ingress_class_name: "traefik".to_string(),
            cluster_issuer: "letsencrypt-production".to_string(),
        }
    }
}

impl ControlPlaneConfig {
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            base_domain: env::var("BASE_DOMAIN").unwrap_or(defaults.base_domain),
            ingress_class_name: env::var("INGRESS_CLASS_NAME")
                .unwrap_or(defaults.ingress_class_name),
            cluster_issuer: env::var("CLUSTER_ISSUER").unwrap_or(defaults.cluster_issuer),
        }
    }
}

#[derive(Clone)]
pub struct ControlPlane {
    state: Arc<AppState>,
}

impl ControlPlane {
    pub fn new(kube: Client, pool: PgPool, config: ControlPlaneConfig) -> Self {
        Self {
            state: Arc::new(AppState {
                kube,
                config,
                store: Store::new(pool),
            }),
        }
    }

    pub fn router(&self) -> Router {
        api::router(Arc::clone(&self.state))
    }

    pub fn admin_router(&self) -> Router {
        api::admin_router(Arc::clone(&self.state))
    }

    pub fn spawn_reconciler(&self) -> JoinHandle<()> {
        tokio::spawn(reconciler::run(Arc::clone(&self.state)))
    }
}
