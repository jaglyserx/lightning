use std::{collections::BTreeMap, fmt};

use crate::ControlPlaneConfig;
use crate::store::AppRecord;
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec, DeploymentStatus},
        core::v1::{
            Container, ContainerPort, Namespace, PodSpec, PodTemplateSpec, Service, ServicePort,
            ServiceSpec,
        },
        networking::v1::{
            HTTPIngressPath, HTTPIngressRuleValue, Ingress, IngressBackend, IngressRule,
            IngressServiceBackend, IngressSpec, IngressTLS, ServiceBackendPort,
        },
    },
    apimachinery::pkg::{
        apis::meta::v1::{LabelSelector, ObjectMeta},
        util::intstr::IntOrString,
    },
};
use kube::{
    Client,
    api::{Api, DeleteParams, Patch, PatchParams, PostParams},
};
use serde::{Deserialize, Serialize};
use sqlx::types::Uuid;

const MANAGER_NAME: &str = "lightning-control-plane";
const NAMESPACE_PREFIX: &str = "lightning-app-";
const APP_ID_LABEL: &str = "lightning.joels.computer/app-id";

#[derive(Debug)]
pub enum DeployErrorKind {
    NotFound,
    Unexpected,
}

#[derive(Debug)]
pub struct DeployError {
    pub kind: DeployErrorKind,
    pub message: String,
}

impl DeployError {
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: DeployErrorKind::NotFound,
            message: message.into(),
        }
    }

    fn unexpected(message: impl Into<String>) -> Self {
        Self {
            kind: DeployErrorKind::Unexpected,
            message: message.into(),
        }
    }
}

impl fmt::Display for DeployError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for DeployError {}

#[derive(Debug, Deserialize)]
pub struct CreateAppRequest {
    pub name: String,
    pub image: String,
    pub port: u16,
    pub hostname: Option<String>,
    pub replicas: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct AppSpec {
    pub app_id: Option<Uuid>,
    pub name: String,
    pub namespace: String,
    pub image: String,
    pub port: u16,
    pub hostname: String,
    pub replicas: i32,
}

#[derive(Debug, Serialize)]
pub struct AppStatusResponse {
    pub name: String,
    pub namespace: String,
    pub hostname: String,
    pub url: String,
    pub image: Option<String>,
    pub replicas: i32,
    pub updated_replicas: i32,
    pub ready_replicas: i32,
    pub available_replicas: i32,
    pub unavailable_replicas: i32,
    pub status: String,
    pub message: String,
}

impl AppSpec {
    pub(crate) fn from_request(
        request: CreateAppRequest,
        base_domain: &str,
    ) -> Result<Self, String> {
        let name = validate_name(&request.name)?;
        let image = request.image.trim().to_string();
        if image.is_empty() {
            return Err("image is required".to_string());
        }

        if request.port == 0 {
            return Err("port must be greater than zero".to_string());
        }

        let replicas = request.replicas.unwrap_or(1);
        if replicas < 1 {
            return Err("replicas must be greater than or equal to 1".to_string());
        }

        let hostname = request
            .hostname
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("{name}.{base_domain}"));

        Ok(Self {
            app_id: None,
            namespace: format!("{NAMESPACE_PREFIX}{name}"),
            name,
            image,
            port: request.port,
            hostname,
            replicas,
        })
    }
}

impl TryFrom<&AppRecord> for AppSpec {
    type Error = String;

    fn try_from(app: &AppRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            app_id: Some(app.id),
            name: app.name.clone(),
            namespace: app.namespace.clone(),
            image: app.image.clone(),
            port: u16::try_from(app.port).map_err(|_| "port is outside the valid range")?,
            hostname: app.hostname.clone(),
            replicas: app.replicas,
        })
    }
}

fn validate_name(input: &str) -> Result<String, String> {
    let name = input.trim().to_lowercase();
    if name.is_empty() {
        return Err("name is required".to_string());
    }

    let max_name_length = 63 - NAMESPACE_PREFIX.len();
    if name.len() > max_name_length {
        return Err(format!("name must be at most {max_name_length} characters"));
    }

    if !name
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err("name must contain only lowercase letters, numbers, and hyphens".to_string());
    }

    if name.starts_with('-') || name.ends_with('-') {
        return Err("name must start and end with an alphanumeric character".to_string());
    }

    Ok(name)
}

fn labels_for(app: &AppSpec) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::from([
        ("app.kubernetes.io/name".to_string(), app.name.clone()),
        (
            "app.kubernetes.io/managed-by".to_string(),
            MANAGER_NAME.to_string(),
        ),
    ]);
    if let Some(app_id) = app.app_id {
        labels.insert(APP_ID_LABEL.to_string(), app_id.to_string());
    }
    labels
}

fn verify_namespace_ownership(namespace: &Namespace, app: &AppSpec) -> Result<(), DeployError> {
    let expected_app_id = app.app_id.ok_or_else(|| {
        DeployError::unexpected(format!("app `{}` has no persisted identity", app.name))
    })?;
    let labels = namespace.metadata.labels.as_ref();
    let managed_by = labels
        .and_then(|labels| labels.get("app.kubernetes.io/managed-by"))
        .map(String::as_str);
    let owner_id = labels
        .and_then(|labels| labels.get(APP_ID_LABEL))
        .map(String::as_str);
    let expected_app_id = expected_app_id.to_string();

    if managed_by == Some(MANAGER_NAME) && owner_id == Some(expected_app_id.as_str()) {
        Ok(())
    } else {
        Err(DeployError::unexpected(format!(
            "refusing to mutate namespace `{}` because it is not owned by app `{}` ({expected_app_id})",
            app.namespace, app.name
        )))
    }
}

fn tls_secret_name(hostname: &str) -> String {
    format!("{}-tls", hostname.replace('.', "-"))
}

fn build_namespace(app: &AppSpec) -> Namespace {
    Namespace {
        metadata: ObjectMeta {
            name: Some(app.namespace.clone()),
            labels: Some(labels_for(app)),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn build_deployment(app: &AppSpec) -> Deployment {
    let labels = labels_for(app);

    Deployment {
        metadata: ObjectMeta {
            name: Some(app.name.clone()),
            namespace: Some(app.namespace.clone()),
            labels: Some(labels.clone()),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(app.replicas),
            selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    containers: vec![Container {
                        name: app.name.clone(),
                        image: Some(app.image.clone()),
                        ports: Some(vec![ContainerPort {
                            container_port: i32::from(app.port),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn build_service(app: &AppSpec) -> Service {
    Service {
        metadata: ObjectMeta {
            name: Some(app.name.clone()),
            namespace: Some(app.namespace.clone()),
            labels: Some(labels_for(app)),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            selector: Some(labels_for(app)),
            ports: Some(vec![ServicePort {
                name: Some("http".to_string()),
                port: i32::from(app.port),
                target_port: Some(IntOrString::Int(i32::from(app.port))),
                ..Default::default()
            }]),
            type_: Some("ClusterIP".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn build_ingress(app: &AppSpec, config: &ControlPlaneConfig) -> Ingress {
    Ingress {
        metadata: ObjectMeta {
            name: Some(app.name.clone()),
            namespace: Some(app.namespace.clone()),
            labels: Some(labels_for(app)),
            annotations: Some(BTreeMap::from([(
                "cert-manager.io/cluster-issuer".to_string(),
                config.cluster_issuer.clone(),
            )])),
            ..Default::default()
        },
        spec: Some(IngressSpec {
            ingress_class_name: Some(config.ingress_class_name.clone()),
            tls: Some(vec![IngressTLS {
                hosts: Some(vec![app.hostname.clone()]),
                secret_name: Some(tls_secret_name(&app.hostname)),
            }]),
            rules: Some(vec![IngressRule {
                host: Some(app.hostname.clone()),
                http: Some(HTTPIngressRuleValue {
                    paths: vec![HTTPIngressPath {
                        path: Some("/".to_string()),
                        path_type: "Prefix".to_string(),
                        backend: IngressBackend {
                            service: Some(IngressServiceBackend {
                                name: app.name.clone(),
                                port: Some(ServiceBackendPort {
                                    number: Some(i32::from(app.port)),
                                    ..Default::default()
                                }),
                            }),
                            ..Default::default()
                        },
                    }],
                }),
            }]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub async fn apply_app(
    client: &Client,
    config: &ControlPlaneConfig,
    app: &AppSpec,
) -> Result<(), DeployError> {
    apply_resources(client, config, app).await?;
    Ok(())
}

pub async fn get_app_status_for_app(
    client: &Client,
    namespace: &str,
    name: &str,
    hostname: &str,
) -> Result<AppStatusResponse, DeployError> {
    build_status_response(client, namespace, name, hostname).await
}

pub async fn delete_app_for_app(
    client: &Client,
    app: &AppSpec,
) -> Result<AppStatusResponse, DeployError> {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let namespace = &app.namespace;
    let name = &app.name;
    let hostname = &app.hostname;

    let Some(existing) = namespaces.get_opt(namespace).await.map_err(|err| {
        DeployError::unexpected(format!("failed to inspect namespace `{namespace}`: {err}"))
    })?
    else {
        return Ok(AppStatusResponse {
            name: name.to_string(),
            namespace: namespace.to_string(),
            hostname: hostname.to_string(),
            url: format!("https://{hostname}"),
            image: None,
            replicas: 0,
            updated_replicas: 0,
            ready_replicas: 0,
            available_replicas: 0,
            unavailable_replicas: 0,
            status: "deleted".to_string(),
            message: "namespace is absent".to_string(),
        });
    };
    verify_namespace_ownership(&existing, app)?;

    match namespaces.delete(namespace, &DeleteParams::default()).await {
        Ok(_) => Ok(AppStatusResponse {
            name: name.to_string(),
            namespace: namespace.to_string(),
            hostname: hostname.to_string(),
            url: format!("https://{hostname}"),
            image: None,
            replicas: 0,
            updated_replicas: 0,
            ready_replicas: 0,
            available_replicas: 0,
            unavailable_replicas: 0,
            status: "deleting".to_string(),
            message: "namespace deletion requested".to_string(),
        }),
        Err(kube::Error::Api(err)) if err.code == 404 => Ok(AppStatusResponse {
            name: name.to_string(),
            namespace: namespace.to_string(),
            hostname: hostname.to_string(),
            url: format!("https://{hostname}"),
            image: None,
            replicas: 0,
            updated_replicas: 0,
            ready_replicas: 0,
            available_replicas: 0,
            unavailable_replicas: 0,
            status: "deleted".to_string(),
            message: "namespace is absent".to_string(),
        }),
        Err(err) => Err(DeployError::unexpected(format!(
            "failed to delete namespace `{namespace}`: {err}"
        ))),
    }
}

async fn apply_resources(
    client: &Client,
    config: &ControlPlaneConfig,
    app: &AppSpec,
) -> Result<(), DeployError> {
    let patch_params = PatchParams::apply(MANAGER_NAME).force();

    let namespaces: Api<Namespace> = Api::all(client.clone());
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), &app.namespace);
    let services: Api<Service> = Api::namespaced(client.clone(), &app.namespace);
    let ingresses: Api<Ingress> = Api::namespaced(client.clone(), &app.namespace);

    let namespace = build_namespace(app);
    let deployment = build_deployment(app);
    let service = build_service(app);
    let ingress = build_ingress(app, config);

    match namespaces.get_opt(&app.namespace).await.map_err(|err| {
        DeployError::unexpected(format!(
            "failed to inspect namespace `{}`: {err}",
            app.namespace
        ))
    })? {
        Some(existing) => {
            verify_namespace_ownership(&existing, app)?;
            namespaces
                .patch(&app.namespace, &patch_params, &Patch::Apply(&namespace))
                .await
                .map_err(|err| {
                    DeployError::unexpected(format!(
                        "failed to apply namespace `{}`: {err}",
                        app.namespace
                    ))
                })?;
        }
        None => {
            namespaces
                .create(&PostParams::default(), &namespace)
                .await
                .map_err(|err| {
                    DeployError::unexpected(format!(
                        "failed to create namespace `{}`: {err}",
                        app.namespace
                    ))
                })?;
        }
    }

    deployments
        .patch(&app.name, &patch_params, &Patch::Apply(&deployment))
        .await
        .map_err(|err| {
            DeployError::unexpected(format!("failed to apply deployment `{}`: {err}", app.name))
        })?;

    services
        .patch(&app.name, &patch_params, &Patch::Apply(&service))
        .await
        .map_err(|err| {
            DeployError::unexpected(format!("failed to apply service `{}`: {err}", app.name))
        })?;

    ingresses
        .patch(&app.name, &patch_params, &Patch::Apply(&ingress))
        .await
        .map_err(|err| {
            DeployError::unexpected(format!("failed to apply ingress `{}`: {err}", app.name))
        })?;

    Ok(())
}

async fn build_status_response(
    client: &Client,
    namespace: &str,
    name: &str,
    fallback_hostname: &str,
) -> Result<AppStatusResponse, DeployError> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let ingresses: Api<Ingress> = Api::namespaced(client.clone(), namespace);
    let deployment = match deployments.get(name).await {
        Ok(deployment) => deployment,
        Err(kube::Error::Api(err)) if err.code == 404 => {
            return Err(DeployError::not_found(format!(
                "app `{name}` was not found"
            )));
        }
        Err(err) => {
            return Err(DeployError::unexpected(format!(
                "failed to fetch deployment `{name}`: {err}"
            )));
        }
    };

    let spec = deployment.spec.as_ref();
    let status = deployment.status.as_ref();
    let desired_replicas = spec.and_then(|value| value.replicas).unwrap_or(0);
    let available_replicas = status
        .and_then(|value| value.available_replicas)
        .unwrap_or(0);
    let updated_replicas = status.and_then(|value| value.updated_replicas).unwrap_or(0);
    let ready_replicas = status.and_then(|value| value.ready_replicas).unwrap_or(0);
    let unavailable_replicas = status
        .and_then(|value| value.unavailable_replicas)
        .unwrap_or(0);
    let image = spec
        .and_then(|value| value.template.spec.as_ref())
        .and_then(|value| value.containers.first())
        .and_then(|value| value.image.clone());
    let hostname = ingresses
        .get(name)
        .await
        .ok()
        .and_then(|ingress| ingress.spec)
        .and_then(|spec| spec.rules)
        .and_then(|rules| rules.first().and_then(|rule| rule.host.clone()))
        .unwrap_or_else(|| fallback_hostname.to_string());

    let deployment_generation = deployment.metadata.generation.unwrap_or(0);
    let (rollout_complete, message) =
        assess_rollout(deployment_generation, desired_replicas, status);

    let computed_status = if rollout_complete { "ready" } else { "warning" };

    Ok(AppStatusResponse {
        name: name.to_string(),
        namespace: namespace.to_string(),
        hostname: hostname.clone(),
        url: format!("https://{hostname}"),
        image,
        replicas: desired_replicas,
        updated_replicas,
        ready_replicas,
        available_replicas,
        unavailable_replicas,
        status: computed_status.to_string(),
        message: if rollout_complete {
            format!("all {desired_replicas} replicas are updated, ready, and available")
        } else {
            message
        },
    })
}

fn assess_rollout(
    deployment_generation: i64,
    desired_replicas: i32,
    status: Option<&DeploymentStatus>,
) -> (bool, String) {
    let observed_generation = status
        .and_then(|value| value.observed_generation)
        .unwrap_or(0);
    let updated = status.and_then(|value| value.updated_replicas).unwrap_or(0);
    let ready = status.and_then(|value| value.ready_replicas).unwrap_or(0);
    let available = status
        .and_then(|value| value.available_replicas)
        .unwrap_or(0);
    let unavailable = status
        .and_then(|value| value.unavailable_replicas)
        .unwrap_or(0);
    let complete = desired_replicas > 0
        && observed_generation >= deployment_generation
        && updated == desired_replicas
        && ready == desired_replicas
        && available == desired_replicas
        && unavailable == 0;

    if complete {
        return (
            true,
            format!("all {desired_replicas} replicas are updated, ready, and available"),
        );
    }

    let condition_warning =
        status
            .and_then(|value| value.conditions.as_ref())
            .and_then(|conditions| {
                conditions.iter().find_map(|condition| {
                    (condition.reason.as_deref() == Some("ProgressDeadlineExceeded")).then(|| {
                        condition
                            .message
                            .clone()
                            .or_else(|| condition.reason.clone())
                            .unwrap_or_else(|| {
                                "deployment exceeded its progress deadline".to_string()
                            })
                    })
                })
            });

    let warning = condition_warning.unwrap_or_else(|| {
        format!(
            "rollout incomplete: generation {observed_generation}/{deployment_generation}, replicas updated {updated}/{desired_replicas}, ready {ready}/{desired_replicas}, available {available}/{desired_replicas}, unavailable {unavailable}"
        )
    });
    (false, warning)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(name: &str) -> CreateAppRequest {
        CreateAppRequest {
            name: name.into(),
            image: "example/web:v1".into(),
            port: 8080,
            hostname: None,
            replicas: None,
        }
    }

    #[test]
    fn request_defaults_match_the_cluster_contract() {
        let app = AppSpec::from_request(request("Hello"), "apps.joels.computer")
            .expect("request should be valid");

        assert_eq!(app.name, "hello");
        assert_eq!(app.namespace, "lightning-app-hello");
        assert_eq!(app.hostname, "hello.apps.joels.computer");
        assert_eq!(app.replicas, 1);
    }

    #[test]
    fn request_rejects_invalid_kubernetes_names() {
        let error = AppSpec::from_request(request("not_valid"), "apps.joels.computer")
            .expect_err("name should be rejected");
        assert!(error.contains("lowercase letters"));
    }

    #[test]
    fn request_name_must_fit_the_prefixed_namespace() {
        let error = AppSpec::from_request(
            request("this-name-is-fifty-characters-long-and-will-not-fit-x"),
            "apps.joels.computer",
        )
        .expect_err("name should be rejected");

        assert!(error.contains("at most 49 characters"));
    }

    #[test]
    fn namespace_ownership_requires_the_persisted_app_identity() {
        let mut app = AppSpec::from_request(request("hello"), "apps.joels.computer")
            .expect("request should be valid");
        let app_id = Uuid::new_v4();
        app.app_id = Some(app_id);

        let namespace = build_namespace(&app);
        verify_namespace_ownership(&namespace, &app).expect("namespace should be owned");

        let mut unowned = namespace;
        unowned
            .metadata
            .labels
            .as_mut()
            .expect("labels")
            .remove(APP_ID_LABEL);
        let error = verify_namespace_ownership(&unowned, &app)
            .expect_err("namespace without app identity must be rejected");
        assert!(error.to_string().contains("refusing to mutate namespace"));
    }

    #[test]
    fn namespace_owned_by_another_app_is_rejected() {
        let mut app = AppSpec::from_request(request("hello"), "apps.joels.computer")
            .expect("request should be valid");
        app.app_id = Some(Uuid::new_v4());
        let mut namespace = build_namespace(&app);
        namespace
            .metadata
            .labels
            .as_mut()
            .expect("labels")
            .insert(APP_ID_LABEL.to_string(), Uuid::new_v4().to_string());

        assert!(verify_namespace_ownership(&namespace, &app).is_err());
    }

    #[test]
    fn rendered_resources_use_the_expected_ingress_and_service_shape() {
        let app = AppSpec::from_request(request("hello"), "apps.joels.computer")
            .expect("request should be valid");
        let config = ControlPlaneConfig {
            base_domain: "apps.joels.computer".into(),
            ingress_class_name: "traefik".into(),
            cluster_issuer: "letsencrypt-production".into(),
        };

        let service = build_service(&app);
        let ingress = build_ingress(&app, &config);

        assert_eq!(service.spec.unwrap().ports.unwrap()[0].port, 8080);
        assert_eq!(
            ingress.spec.as_ref().unwrap().ingress_class_name.as_deref(),
            Some("traefik")
        );
        assert_eq!(
            ingress.metadata.annotations.unwrap()["cert-manager.io/cluster-issuer"],
            "letsencrypt-production"
        );
    }

    #[test]
    fn old_available_replicas_do_not_complete_a_new_rollout() {
        let status = DeploymentStatus {
            observed_generation: Some(2),
            replicas: Some(2),
            updated_replicas: Some(1),
            ready_replicas: Some(1),
            available_replicas: Some(1),
            unavailable_replicas: Some(1),
            ..Default::default()
        };

        let (complete, warning) = assess_rollout(2, 1, Some(&status));
        assert!(!complete);
        assert!(warning.contains("updated 1/1"));
        assert!(warning.contains("unavailable 1"));
    }

    #[test]
    fn rollout_completes_only_when_every_replica_is_current_and_available() {
        let status = DeploymentStatus {
            observed_generation: Some(3),
            replicas: Some(2),
            updated_replicas: Some(2),
            ready_replicas: Some(2),
            available_replicas: Some(2),
            unavailable_replicas: Some(0),
            ..Default::default()
        };

        let (complete, message) = assess_rollout(3, 2, Some(&status));
        assert!(complete);
        assert_eq!(message, "all 2 replicas are updated, ready, and available");
    }
}
