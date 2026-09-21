# agentjail — one-shot DevX.
#
#   make setup    install web deps, generate a dev API key
#   make dev      start the full stack (control plane + phantom proxy + web)
#   make test     run every unit test (Rust + Node + Python)
#   make build    production builds (server binary + web bundle + SDKs)
#   make logs     tail container logs
#   make down     stop + remove containers (volumes kept)
#   make e2e      run scripts/e2e-workspaces.sh against the running stack
#   make clean    wipe build artifacts
#   make doctor   diagnose prerequisites, ports, and running services

SHELL        := /bin/bash
ENV_FILE     := .env.local
COMPOSE      := docker compose -f docker-compose.platform.yml
WEB_DIR      := web
SDK_NODE_DIR := packages/sdk-node
SDK_PY_DIR   := packages/sdk-python

# Prefer bun if present, fall back to npm.
PKG := $(shell command -v bun >/dev/null 2>&1 && echo bun || echo npm)

# Terminal colors (fall back to no-color if not a tty).
ifneq (,$(findstring xterm,$(TERM)))
  C_RESET := \033[0m
  C_BOLD  := \033[1m
  C_DIM   := \033[2m
  C_GREEN := \033[32m
  C_AMBER := \033[33m
  C_RED   := \033[31m
  C_CYAN  := \033[36m
endif

.DEFAULT_GOAL := help
.PHONY: help setup dev doctor test build logs down e2e clean demos \
        test-rust test-node test-python build-rust build-web build-sdks

help:
	@printf "$(C_BOLD)agentjail$(C_RESET) $(C_DIM)— phantom control plane$(C_RESET)\n\n"
	@printf "  $(C_GREEN)make setup$(C_RESET)    install deps, generate dev API key\n"
	@printf "  $(C_GREEN)make dev$(C_RESET)      start the full stack (server + web)\n"
	@printf "  $(C_GREEN)make test$(C_RESET)     run every unit test (Rust + Node + Python)\n"
	@printf "  $(C_GREEN)make build$(C_RESET)    production builds (server + web + SDKs)\n"
	@printf "  $(C_GREEN)make logs$(C_RESET)     tail docker logs\n"
	@printf "  $(C_GREEN)make down$(C_RESET)     stop containers (volumes kept)\n"
	@printf "  $(C_GREEN)make e2e$(C_RESET)      run the workspaces + snapshots smoke script\n"
	@printf "  $(C_GREEN)make demos$(C_RESET)    list the runnable pattern demos\n"
	@printf "  $(C_GREEN)make clean$(C_RESET)    wipe build artifacts\n"
	@printf "  $(C_GREEN)make doctor$(C_RESET)   check prerequisites and running services\n\n"

# ─── setup ──────────────────────────────────────────────────────────────────

setup:
	@printf "$(C_BOLD)→ setup$(C_RESET)\n"
	@command -v docker >/dev/null 2>&1 || { printf "  $(C_RED)✗$(C_RESET) docker not found — install Docker Desktop\n"; exit 1; }
	@command -v $(PKG) >/dev/null 2>&1 || { printf "  $(C_RED)✗$(C_RESET) $(PKG) not found\n"; exit 1; }
	@printf "  $(C_GREEN)✓$(C_RESET) docker\n"
	@printf "  $(C_GREEN)✓$(C_RESET) $(PKG) $$($(PKG) --version | head -1)\n"
	@printf "  $(C_DIM)→$(C_RESET) installing web deps (via $(PKG))…\n"
	@cd $(WEB_DIR) && $(PKG) install --silent 2>&1 | tail -5 || cd $(WEB_DIR) && $(PKG) install
	@if [ ! -f $(ENV_FILE) ]; then \
	  KEY="aj_local_$$(openssl rand -hex 16 2>/dev/null || date +%s%N | sha256sum | head -c 32)"; \
	  printf "AGENTJAIL_API_KEY=%s\n" "$$KEY" > $(ENV_FILE); \
	  printf "PROXY_BASE_URL=http://server:8443\n" >> $(ENV_FILE); \
	  printf "DATABASE_URL=postgres://agentjail:agentjail@postgres:5432/agentjail\n" >> $(ENV_FILE); \
	  printf "  $(C_GREEN)✓$(C_RESET) wrote $(ENV_FILE) with a fresh API key + DATABASE_URL\n"; \
	else \
	  printf "  $(C_DIM)•$(C_RESET) $(ENV_FILE) already exists (left as-is)\n"; \
	fi
	@printf "\n  $(C_BOLD)next:$(C_RESET) $(C_CYAN)make dev$(C_RESET)\n\n"

# ─── dev ────────────────────────────────────────────────────────────────────

dev:
	@if [ ! -f $(ENV_FILE) ]; then \
	  printf "$(C_AMBER)!$(C_RESET) no $(ENV_FILE) — run $(C_CYAN)make setup$(C_RESET) first\n"; exit 1; \
	fi
	@printf "$(C_BOLD)→ dev$(C_RESET)  $(C_DIM)control plane + phantom proxy (docker) · web (vite HMR)$(C_RESET)\n"
	@printf "  $(C_DIM)recreating server container so sqlx migrations + new code land on every run$(C_RESET)\n"
	@set -a; source $(ENV_FILE); set +a; \
	  $(COMPOSE) up -d --build --force-recreate server 2>&1 | tail -10 ; \
	  printf "\n  $(C_GREEN)✓$(C_RESET) control plane  $(C_CYAN)http://localhost:7070$(C_RESET)\n"; \
	  printf "  $(C_GREEN)✓$(C_RESET) phantom proxy  $(C_CYAN)http://localhost:8443$(C_RESET)\n"; \
	  printf "  $(C_GREEN)✓$(C_RESET) web (HMR)      $(C_CYAN)http://localhost:5173$(C_RESET)  $(C_BOLD)← open this$(C_RESET)\n"; \
	  printf "\n  $(C_DIM)api key is in $(ENV_FILE) — paste it on the login screen$(C_RESET)\n"; \
	  printf "  $(C_DIM)ctrl-c stops the server container and vite dev$(C_RESET)\n\n"; \
	  trap '$(COMPOSE) stop server >/dev/null 2>&1' INT TERM EXIT; \
	  cd $(WEB_DIR) && $(PKG) run dev

# ─── doctor ─────────────────────────────────────────────────────────────────

doctor:
	@printf "$(C_BOLD)→ doctor$(C_RESET)\n\n"
	@printf "$(C_DIM)tooling$(C_RESET)\n"
	@$(call probe_bin,docker)
	@$(call probe_bin,$(PKG))
	@$(call probe_bin,openssl)
	@$(call probe_bin,curl)
	@printf "\n$(C_DIM)docker daemon$(C_RESET)\n"
	@if docker info >/dev/null 2>&1; then \
	  printf "  $(C_GREEN)✓$(C_RESET) running\n"; \
	else \
	  printf "  $(C_RED)✗$(C_RESET) not reachable — start Docker Desktop\n"; \
	fi
	@printf "\n$(C_DIM)ports$(C_RESET)\n"
	@$(call probe_port,3000,web ui)
	@$(call probe_port,5173,vite dev)
	@$(call probe_port,5433,postgres)
	@$(call probe_port,7070,control plane)
	@$(call probe_port,8443,phantom proxy)
	@printf "\n$(C_DIM)services$(C_RESET)\n"
	@if docker exec agentjail-postgres-1 pg_isready -U agentjail -d agentjail >/dev/null 2>&1; then \
	  CREDS=$$(docker exec agentjail-postgres-1 psql -U agentjail -d agentjail -At -c "SELECT COUNT(*) FROM credentials" 2>/dev/null); \
	  JAILS=$$(docker exec agentjail-postgres-1 psql -U agentjail -d agentjail -At -c "SELECT COUNT(*) FROM jails"       2>/dev/null); \
	  printf "  $(C_GREEN)✓$(C_RESET) postgres healthy     $(C_DIM)(creds=%s jails=%s)$(C_RESET)\n" "$${CREDS:-?}" "$${JAILS:-?}"; \
	else \
	  printf "  $(C_AMBER)•$(C_RESET) postgres not reachable\n"; \
	fi
	@if curl -fsS --max-time 1 http://localhost:7070/healthz >/dev/null 2>&1; then \
	  TOTAL=$$(curl -fsS --max-time 1 http://localhost:7070/v1/stats 2>/dev/null | grep -oE '"total_execs":[0-9]+' | cut -d: -f2); \
	  printf "  $(C_GREEN)✓$(C_RESET) control plane alive  $(C_DIM)(total_execs=%s)$(C_RESET)\n" "$${TOTAL:-?}"; \
	else \
	  printf "  $(C_AMBER)•$(C_RESET) control plane not responding on :7070\n"; \
	fi
	@if curl -fsS --max-time 1 http://localhost:3000 >/dev/null 2>&1; then \
	  printf "  $(C_GREEN)✓$(C_RESET) web (prod) alive on :3000\n"; \
	else \
	  printf "  $(C_AMBER)•$(C_RESET) web (prod) not responding on :3000\n"; \
	fi
	@printf "\n$(C_DIM)schema$(C_RESET)\n"
	@MIG_DIR=crates/agentjail-ctl/migrations; REG_FILE=crates/agentjail-ctl/src/db/mod.rs; \
	 DISK_TMP=$$(mktemp); REG_TMP=$$(mktemp); \
	 ls $$MIG_DIR/*.sql 2>/dev/null | xargs -n1 basename 2>/dev/null | sed 's/\.sql$$//' | sort -u > $$DISK_TMP; \
	 grep -oE '\("[0-9]+_[a-z_]+"' $$REG_FILE 2>/dev/null | tr -d '("' | sort -u > $$REG_TMP; \
	 DISK_COUNT=$$(wc -l < $$DISK_TMP | tr -d ' '); \
	 REG_COUNT=$$(wc -l < $$REG_TMP | tr -d ' '); \
	 UNREG=$$(grep -Fxv -f $$REG_TMP  $$DISK_TMP | tr '\n' ' ' | sed 's/ *$$//'); \
	 ORPHAN=$$(grep -Fxv -f $$DISK_TMP $$REG_TMP | tr '\n' ' ' | sed 's/ *$$//'); \
	 if [ -n "$$UNREG" ]; then \
	   printf "  $(C_RED)✗$(C_RESET) files not registered in db/mod.rs: $(C_AMBER)%s$(C_RESET)\n" "$$UNREG"; \
	 elif [ -n "$$ORPHAN" ]; then \
	   printf "  $(C_RED)✗$(C_RESET) registered but no file on disk: $(C_AMBER)%s$(C_RESET)\n" "$$ORPHAN"; \
	 else \
	   printf "  $(C_GREEN)✓$(C_RESET) migrations registered  $(C_DIM)(%s files = %s registrations)$(C_RESET)\n" "$$DISK_COUNT" "$$REG_COUNT"; \
	 fi; \
	 rm -f $$DISK_TMP $$REG_TMP
	@if docker inspect agentjail-server-1 >/dev/null 2>&1; then \
	  CONT_START=$$(docker inspect --format '{{.State.StartedAt}}' agentjail-server-1 2>/dev/null); \
	  CONT_EPOCH=$$(date -u -j -f "%Y-%m-%dT%H:%M:%S" "$${CONT_START%%.*}" +%s 2>/dev/null || date -u -d "$$CONT_START" +%s 2>/dev/null); \
	  NEWEST_FILE=$$(ls -t crates/agentjail-ctl/migrations/*.sql 2>/dev/null | head -1); \
	  FILE_EPOCH=$$(stat -f %m "$$NEWEST_FILE" 2>/dev/null || stat -c %Y "$$NEWEST_FILE" 2>/dev/null); \
	  if [ -n "$$CONT_EPOCH" ] && [ -n "$$FILE_EPOCH" ]; then \
	    if [ "$$FILE_EPOCH" -gt "$$CONT_EPOCH" ]; then \
	      AGE=$$(( FILE_EPOCH - CONT_EPOCH )); \
	      printf "  $(C_RED)✗$(C_RESET) server started BEFORE newest migration  $(C_AMBER)(%s newer by %ds)$(C_RESET)\n" "$$(basename $$NEWEST_FILE)" "$$AGE"; \
	      printf "    $(C_DIM)run $(C_CYAN)make dev$(C_RESET)$(C_DIM) to recreate the server container$(C_RESET)\n"; \
	    else \
	      printf "  $(C_GREEN)✓$(C_RESET) server up-to-date      $(C_DIM)(container newer than $$(basename $$NEWEST_FILE))$(C_RESET)\n"; \
	    fi; \
	  else \
	    printf "  $(C_AMBER)•$(C_RESET) couldn't compare container vs. file mtime\n"; \
	  fi; \
	else \
	  printf "  $(C_AMBER)•$(C_RESET) server container not running — $(C_CYAN)make dev$(C_RESET)\n"; \
	fi
	@printf "\n$(C_DIM)config$(C_RESET)\n"
	@if [ -f $(ENV_FILE) ]; then \
	  KEY=$$(grep -E '^AGENTJAIL_API_KEY=' $(ENV_FILE) | cut -d= -f2); \
	  printf "  $(C_GREEN)✓$(C_RESET) $(ENV_FILE) present  $(C_DIM)(key=%s…)$(C_RESET)\n" "$${KEY:0:16}"; \
	else \
	  printf "  $(C_AMBER)•$(C_RESET) no $(ENV_FILE) — run $(C_CYAN)make setup$(C_RESET)\n"; \
	fi
	@printf "\n"

# ─── test ───────────────────────────────────────────────────────────────────

test: test-rust test-node test-python
	@printf "\n  $(C_GREEN)✓$(C_RESET) all unit tests passed\n\n"

test-rust:
	@printf "$(C_BOLD)→ test-rust$(C_RESET)  $(C_DIM)cargo test (requires Linux — runs inside rust:bookworm docker)$(C_RESET)\n"
	@docker run --rm -v "$$PWD":/src -w /src rust:1.88-slim-bookworm bash -c '\
	  apt-get update -qq && \
	  apt-get install -y --no-install-recommends pkg-config libssl-dev libseccomp-dev >/dev/null 2>&1 && \
	  cargo test -p agentjail-ctl --lib && \
	  cargo test -p agentjail --test snapshot_test && \
	  cargo check -p agentjail-server' | tail -20

# Privileged run — exercises every integration/security test. Needs
# CAP_SYS_ADMIN for unshare/mount/seccomp, hence --privileged. Runs
# one test at a time because they share cgroup paths and veth IDs.
# On OrbStack / Docker Desktop, host→jail veth routing may fail in the
# container's netns (inbound_reach_test) — that's environmental, not
# a code bug.
test-rust-privileged:
	@printf "$(C_BOLD)→ test-rust-privileged$(C_RESET)  $(C_DIM)full agentjail suite under --privileged docker$(C_RESET)\n"
	@docker run --rm --privileged -v "$$PWD":/src -w /src rust:1.88-slim-bookworm bash -c '\
	  apt-get update -qq && \
	  apt-get install -y --no-install-recommends pkg-config libssl-dev libseccomp-dev python3 >/dev/null 2>&1 && \
	  cargo test -p agentjail \
	    --test security_test \
	    --test malicious_test \
	    --test seccomp_blocklist_test \
	    --test integration \
	    --test fork_test \
	    --test audit_regression_test \
	    --test snapshot_test \
	    -- --test-threads=1'

# End-to-end clone-jail test. Separate target because the tests are
# `#[ignore]`-gated (need privileged docker + internet) and exercise
# the agentjail-ctl crate, not agentjail directly. `git` + CA bundle
# are installed so the jail's rootfs has them via the bind-mount of
# `/usr/bin` and `/etc/ssl`.
#
# Two tests run:
#
#   host_clone_mode_still_works_as_fallback       — expected green
#                                                   anywhere with internet.
#   clone_jail_clones_a_small_public_repo         — green on real Linux
#                                                   hosts / CI runners;
#                                                   fails on OrbStack /
#                                                   Docker Desktop due
#                                                   to nested-virt netns
#                                                   restrictions (the
#                                                   per-jail allowlist
#                                                   proxy can't bind).
#                                                   See the test docstring.
test-rust-privileged-clone:
	@printf "$(C_BOLD)→ test-rust-privileged-clone$(C_RESET)  $(C_DIM)clone-jail end-to-end under --privileged docker$(C_RESET)\n"
	@docker run --rm --privileged -v "$$PWD":/src -w /src \
	  -e RUST_LOG=agentjail=debug,agentjail_ctl=debug \
	  rust:1.88-slim-bookworm bash -c '\
	  apt-get update -qq && \
	  apt-get install -y --no-install-recommends pkg-config libssl-dev libseccomp-dev git ca-certificates iproute2 >/dev/null 2>&1 && \
	  cargo test -p agentjail-ctl --test clone_jail_test -- --ignored --test-threads=1 --nocapture 2>&1'

test-node:
	@printf "$(C_BOLD)→ test-node$(C_RESET)  $(C_DIM)@agentjail/sdk$(C_RESET)\n"
	@cd $(SDK_NODE_DIR) && $(PKG) install --silent >/dev/null 2>&1 && $(PKG) test 2>&1 | tail -10

test-python:
	@printf "$(C_BOLD)→ test-python$(C_RESET)  $(C_DIM)agentjail (pypi)$(C_RESET)\n"
	@cd $(SDK_PY_DIR) && \
	  if [ ! -d .venv ]; then python3 -m venv .venv --clear >/dev/null 2>&1; fi && \
	  .venv/bin/pip install -q -e '.[dev]' >/dev/null 2>&1 && \
	  .venv/bin/pytest 2>&1 | tail -5

# ─── build ──────────────────────────────────────────────────────────────────

build: build-rust build-web build-sdks
	@printf "\n  $(C_GREEN)✓$(C_RESET) build done\n\n"

build-rust:
	@printf "$(C_BOLD)→ build-rust$(C_RESET)  $(C_DIM)cargo build --release (docker-linux)$(C_RESET)\n"
	@docker build -f Dockerfile.server -t agentjail-server:dev . 2>&1 | tail -8

build-web:
	@printf "$(C_BOLD)→ build-web$(C_RESET)  $(C_DIM)vite production bundle$(C_RESET)\n"
	@cd $(WEB_DIR) && $(PKG) run build 2>&1 | tail -6

build-sdks:
	@printf "$(C_BOLD)→ build-sdks$(C_RESET)  $(C_DIM)node sdk + python wheel$(C_RESET)\n"
	@cd $(SDK_NODE_DIR) && $(PKG) run build 2>&1 | tail -3
	@cd $(SDK_PY_DIR) && \
	  if [ ! -d .venv ]; then python3 -m venv .venv --clear >/dev/null 2>&1; fi && \
	  .venv/bin/pip install -q hatchling build >/dev/null 2>&1 && \
	  .venv/bin/python -m build --wheel --outdir dist 2>&1 | tail -3

# ─── ops ────────────────────────────────────────────────────────────────────

logs:
	@$(COMPOSE) logs -f --tail=100

down:
	@printf "$(C_BOLD)→ down$(C_RESET)  $(C_DIM)stopping + removing containers (volumes kept)$(C_RESET)\n"
	@$(COMPOSE) down 2>&1 | tail -5

e2e:
	@if ! curl -fsS --max-time 1 http://localhost:7070/healthz >/dev/null 2>&1; then \
	  printf "$(C_AMBER)!$(C_RESET) control plane not reachable on :7070 — run $(C_CYAN)make dev$(C_RESET) first\n"; exit 1; \
	fi
	@command -v bun >/dev/null 2>&1 || { printf "$(C_RED)✗$(C_RESET) bun not found — install from https://bun.sh\n"; exit 1; }
	@printf "$(C_BOLD)→ e2e$(C_RESET)  $(C_DIM)workspaces + snapshots + fork smoke (via @agentjail/sdk)$(C_RESET)\n"
	@set -a; source $(ENV_FILE); set +a; \
	  CTL_URL=http://localhost:7070 bun scripts/e2e-workspaces.ts

demos:
	@printf "$(C_BOLD)agentjail demos$(C_RESET) $(C_DIM)— runnable SDK patterns$(C_RESET)\n\n"
	@printf "  $(C_CYAN)bun scripts/demos/1-ai-assistant.ts$(C_RESET)      persistent workspace + idle pause\n"
	@printf "  $(C_CYAN)bun scripts/demos/2-review-bot.ts$(C_RESET)        git clone + multi-exec + decision\n"
	@printf "  $(C_CYAN)bun scripts/demos/3-background-agent.ts$(C_RESET)  N-way fork + parallel tasks\n"
	@printf "  $(C_CYAN)bun scripts/demos/4-app-builder.ts$(C_RESET)       dev server + gateway domain\n\n"
	@printf "  $(C_DIM)prereqs: $(C_CYAN)make dev$(C_RESET)$(C_DIM) running; $(C_CYAN)bun$(C_RESET)$(C_DIM) on PATH; AGENTJAIL_API_KEY in $(ENV_FILE)$(C_RESET)\n\n"
	@printf "  $(C_DIM)env overrides: DEMO_REPO=... DEMO_REF=... CTL_URL=http://localhost:7070$(C_RESET)\n\n"

clean:
	@printf "$(C_BOLD)→ clean$(C_RESET)  $(C_DIM)wiping build artifacts$(C_RESET)\n"
	@rm -rf target $(WEB_DIR)/dist $(WEB_DIR)/node_modules
	@rm -rf $(SDK_NODE_DIR)/dist $(SDK_NODE_DIR)/node_modules
	@rm -rf $(SDK_PY_DIR)/dist $(SDK_PY_DIR)/.venv $(SDK_PY_DIR)/src/agentjail/__pycache__ $(SDK_PY_DIR)/tests/__pycache__
	@printf "  $(C_GREEN)✓$(C_RESET) clean\n\n"

# ─── helpers ────────────────────────────────────────────────────────────────

define probe_bin
	if command -v $(1) >/dev/null 2>&1; then \
	  V=$$($(1) --version 2>&1 | head -1 | tr -d '\n'); \
	  printf "  $(C_GREEN)✓$(C_RESET) %-8s  $(C_DIM)%s$(C_RESET)\n" "$(1)" "$$V"; \
	else \
	  printf "  $(C_RED)✗$(C_RESET) %-8s  $(C_DIM)not installed$(C_RESET)\n" "$(1)"; \
	fi
endef

define probe_port
	if lsof -nP -iTCP:$(1) -sTCP:LISTEN >/dev/null 2>&1; then \
	  OWNER=$$(lsof -nP -iTCP:$(1) -sTCP:LISTEN 2>/dev/null | awk 'NR==2 {print $$1}'); \
	  printf "  $(C_AMBER)•$(C_RESET) :%-5s  $(C_DIM)taken by %s ($(2))$(C_RESET)\n" "$(1)" "$$OWNER"; \
	else \
	  printf "  $(C_GREEN)✓$(C_RESET) :%-5s  $(C_DIM)free ($(2))$(C_RESET)\n" "$(1)"; \
	fi
endef
