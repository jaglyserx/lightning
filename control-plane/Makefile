LOCAL_DB_CONTAINER ?= lightning-postgres
LOCAL_DB_PORT ?= 5432
LOCAL_DB_NAME ?= lightning
LOCAL_DB_USER ?= lightning
LOCAL_DB_PASSWORD ?= lightning
LOCAL_DATABASE_URL ?= postgres://$(LOCAL_DB_USER):$(LOCAL_DB_PASSWORD)@localhost:$(LOCAL_DB_PORT)/$(LOCAL_DB_NAME)

.PHONY: help db-up db-wait db-migrate sqlx-prepare sqlx-check db-down

help:
	@printf "Targets:\n"
	@printf "  make db-up          Start local Postgres for SQLx/dev work\n"
	@printf "  make db-migrate     Run local control-plane migrations\n"
	@printf "  make sqlx-prepare   Start local DB, migrate, and update .sqlx metadata\n"
	@printf "  make sqlx-check     Run tests using SQLx offline metadata\n"
	@printf "  make db-down        Stop local Postgres\n"
	@printf "\n"
	@printf "Variables:\n"
	@printf "  LOCAL_DB_PORT=5432  Local Postgres host port\n"

db-up:
	@if docker ps --format '{{.Names}}' | grep -qx '$(LOCAL_DB_CONTAINER)'; then \
		printf "local Postgres already running: $(LOCAL_DB_CONTAINER)\n"; \
	elif docker ps -a --format '{{.Names}}' | grep -qx '$(LOCAL_DB_CONTAINER)'; then \
		docker start $(LOCAL_DB_CONTAINER); \
	else \
		docker run --rm -d \
			--name $(LOCAL_DB_CONTAINER) \
			-e POSTGRES_DB=$(LOCAL_DB_NAME) \
			-e POSTGRES_USER=$(LOCAL_DB_USER) \
			-e POSTGRES_PASSWORD=$(LOCAL_DB_PASSWORD) \
			-p $(LOCAL_DB_PORT):5432 \
			postgres:17-bookworm; \
	fi

db-wait: db-up
	@until docker exec $(LOCAL_DB_CONTAINER) pg_isready -U $(LOCAL_DB_USER) -d $(LOCAL_DB_NAME) >/dev/null 2>&1; do \
		printf "waiting for local Postgres...\n"; \
		sleep 1; \
	done

db-migrate: db-wait
	DATABASE_URL='$(LOCAL_DATABASE_URL)' cargo run --bin migrate

sqlx-prepare: db-migrate
	DATABASE_URL='$(LOCAL_DATABASE_URL)' cargo sqlx prepare

sqlx-check:
	SQLX_OFFLINE=true cargo test

db-down:
	-docker stop $(LOCAL_DB_CONTAINER)
