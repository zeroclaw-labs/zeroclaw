COMPOSE_FILE := docker-compose.dev-codex-discord.yml
ENV_FILE := .env
ENV_TEMPLATE := .env.codex-discord.example
SERVICE := zeroclaw-dev
AUTH_FILE := .zeroclaw/auth-profiles.json

.PHONY: env auth dev down logs logs-debug ps traces tests

TEST_ARGS ?= --locked --verbose

env:
	@if [ ! -f "$(ENV_FILE)" ]; then \
		cp "$(ENV_TEMPLATE)" "$(ENV_FILE)"; \
		echo "Created $(ENV_FILE) from $(ENV_TEMPLATE). Fill required values and rerun make dev."; \
		exit 1; \
	fi

auth: env
	@mkdir -p .zeroclaw
	@if [ -f "$(AUTH_FILE)" ] && rg -q '"provider"[[:space:]]*:[[:space:]]*"openai-codex"' "$(AUTH_FILE)"; then \
		echo "OpenAI Codex auth profile already exists in $(AUTH_FILE)."; \
	else \
		echo "No OpenAI Codex auth profile found. Starting device-code login..."; \
		docker compose --env-file "$(ENV_FILE)" -f "$(COMPOSE_FILE)" run --rm --no-deps "$(SERVICE)" \
			/bin/bash -c 'if [ -n "$${ZEROCLAW_CARGO_FEATURES:-}" ]; then cargo run --features "$${ZEROCLAW_CARGO_FEATURES}" -- auth login --provider openai-codex --device-code; else cargo run -- auth login --provider openai-codex --device-code; fi'; \
	fi

dev-auth: auth
	docker compose --env-file "$(ENV_FILE)" -f "$(COMPOSE_FILE)" up --build --force-recreate

dev: env
	docker compose --env-file "$(ENV_FILE)" -f "$(COMPOSE_FILE)" up --build --force-recreate

up: env
	docker compose --env-file "$(ENV_FILE)" -f "$(COMPOSE_FILE)" up -d --force-recreate

down: env
	docker compose --env-file "$(ENV_FILE)" -f "$(COMPOSE_FILE)" down

logs: env
	docker compose --env-file "$(ENV_FILE)" -f "$(COMPOSE_FILE)" logs -f "$(SERVICE)"

logs-debug: logs

traces: env
	docker compose --env-file "$(ENV_FILE)" -f "$(COMPOSE_FILE)" exec "$(SERVICE)" \
		sh -c 'zeroclaw doctor traces --limit 60 || true'

ps: env
	docker compose --env-file "$(ENV_FILE)" -f "$(COMPOSE_FILE)" ps

tests: env
	docker compose --env-file "$(ENV_FILE)" -f "$(COMPOSE_FILE)" run --rm --no-deps "$(SERVICE)" \
		/bin/bash -c 'if [ -n "$${ZEROCLAW_CARGO_FEATURES:-}" ]; then cargo test --features "$${ZEROCLAW_CARGO_FEATURES}" $(TEST_ARGS); else cargo test $(TEST_ARGS); fi'
