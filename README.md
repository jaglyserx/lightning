# Lightning

Lightning is a small Rust control plane that reconciles application desired state into Kubernetes resources.

Published images are available from `ghcr.io/jaglyserx/lightning`.

It accepts pre-built container images, persists application and deployment-run state in Postgres, and manages a namespace, Deployment, Service, and Ingress for each application through the Kubernetes API.

## HTTP API

Application operations are exposed through the versioned API:

```text
POST   /v1/apps
GET    /v1/apps/{name}
GET    /v1/apps/{name}/status
DELETE /v1/apps/{name}
```

`GET /healthz` reports process liveness. `GET /readyz` reports readiness only when both Postgres and the Kubernetes API are reachable.

Requests are limited to 64 KiB and 30 seconds. API errors use a stable envelope:

```json
{
  "error": {
    "code": "bad_request",
    "message": "name is required"
  }
}
```

The API is not ready for public exposure until bearer-token authentication and namespace ownership protections are implemented.

## Repository layout

```text
control-plane/  Rust API, persistence, migrations, and reconciler
sdk/python/     Python SDK (planned)
openapi/        Public API contract (planned)
```

Cluster provisioning and environment-specific deployment live in the separate private `lightning-infra` repository.

## Development

```bash
cd control-plane
SQLX_OFFLINE=true cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Run `make -C control-plane help` for local Postgres and migration targets. See [plan.md](plan.md) for the current product phase.
