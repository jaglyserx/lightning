# Lightning

Lightning is a small Rust control plane that reconciles application desired state into Kubernetes resources.

Published images are available from `ghcr.io/jaglyserx/lightning`.

It accepts pre-built container images, persists application and deployment-run state in Postgres, and manages a namespace, Deployment, Service, and Ingress for each application through the Kubernetes API.

## HTTP API

Application operations are exposed through the authenticated, versioned API:

```text
POST   /v1/apps
GET    /v1/apps/{name}
GET    /v1/apps/{name}/status
DELETE /v1/apps/{name}
```

Clients authenticate with an opaque bearer token:

```http
Authorization: Bearer ltn_<secret>
```

Tokens contain 256 bits of random secret material. Lightning stores only a SHA-256 hash; plaintext is returned once at creation time.

Token administration is served separately on an internal API listener. By default it binds only to `127.0.0.1:8001`, is absent from the public router, and is unavailable through the pod network. It must not be exposed through a Service or ingress. Reach it through Kubernetes port forwarding:

```bash
kubectl -n <namespace> port-forward deployment/<deployment> 8001:8001

curl -X POST http://127.0.0.1:8001/v1/tokens \
  -H 'Content-Type: application/json' \
  -d '{"name":"local-sdk"}'

curl http://127.0.0.1:8001/v1/tokens

curl -X DELETE http://127.0.0.1:8001/v1/tokens/<token-id>
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

Namespace ownership and derived-hostname protections are enforced by the control plane. Public exposure remains an infrastructure step and should retain bearer authentication, HTTPS, and rate limiting.

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
