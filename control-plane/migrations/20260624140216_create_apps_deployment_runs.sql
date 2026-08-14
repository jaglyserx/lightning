CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE apps (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL UNIQUE,
    namespace text NOT NULL UNIQUE,
    image text NOT NULL,
    port integer NOT NULL CHECK (port > 0 AND port <= 65535),
    hostname text NOT NULL UNIQUE,
    replicas integer NOT NULL CHECK (replicas >= 0),
    desired_state text NOT NULL CHECK (desired_state IN ('active', 'deleted')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX apps_desired_state_idx ON apps (desired_state);

CREATE TABLE deployment_runs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id uuid NOT NULL REFERENCES apps (id) ON DELETE CASCADE,
    trigger_kind text NOT NULL CHECK (trigger_kind IN ('api', 'reconciler')),
    status text NOT NULL CHECK (status IN ('pending', 'running', 'succeeded', 'failed')),
    status_message text,
    started_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz
);

CREATE INDEX deployment_runs_app_id_started_at_idx
    ON deployment_runs (app_id, started_at DESC);
