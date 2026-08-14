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

const SUPPORTED_BASE_DOMAIN: &str = "apps.joels.computer";

#[derive(Clone)]
pub(crate) struct AppState {
    kube: Client,
    config: ControlPlaneConfig,
    store: Store,
}

#[derive(Clone, Debug)]
pub struct ControlPlaneConfig {
    pub(crate) base_domain: String,
    pub(crate) ingress_class_name: String,
    pub(crate) cluster_issuer: String,
}

impl Default for ControlPlaneConfig {
    fn default() -> Self {
        Self {
            base_domain: SUPPORTED_BASE_DOMAIN.to_string(),
            ingress_class_name: "traefik".to_string(),
            cluster_issuer: "letsencrypt-production".to_string(),
        }
    }
}

impl ControlPlaneConfig {
    pub fn from_env() -> Result<Self, String> {
        let defaults = Self::default();
        let config = Self {
            base_domain: env::var("BASE_DOMAIN").unwrap_or(defaults.base_domain),
            ingress_class_name: env::var("INGRESS_CLASS_NAME")
                .unwrap_or(defaults.ingress_class_name),
            cluster_issuer: env::var("CLUSTER_ISSUER").unwrap_or(defaults.cluster_issuer),
        };
        config.validate()
    }

    fn validate(self) -> Result<Self, String> {
        if self.base_domain != SUPPORTED_BASE_DOMAIN {
            return Err(format!(
                "BASE_DOMAIN must be `{SUPPORTED_BASE_DOMAIN}` while custom domains are unsupported"
            ));
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_base_domain_must_match_the_supported_app_boundary() {
        let config = ControlPlaneConfig {
            base_domain: "example.com".into(),
            ..Default::default()
        };

        let error = config
            .validate()
            .expect_err("custom domain must be rejected");
        assert_eq!(
            error,
            "BASE_DOMAIN must be `apps.joels.computer` while custom domains are unsupported"
        );
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
