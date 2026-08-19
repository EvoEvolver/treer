set shell := ["zsh", "-cu"]

test-db-up:
    if docker inspect treer-postgres-test >/dev/null 2>&1; then docker start treer-postgres-test >/dev/null; else docker run --name treer-postgres-test -e POSTGRES_PASSWORD=treer -e POSTGRES_USER=treer -e POSTGRES_DB=treer_test -p 127.0.0.1:55432:5432 -d postgres:17-alpine >/dev/null; fi

test-db-down:
    docker rm -f treer-postgres-test

fmt:
    cargo fmt --all

test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets -- -D warnings

check:
    node scripts/check-docs.mjs
    cd web && pnpm typecheck
    cd web && pnpm build
    cargo fmt --all -- --check
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings

proxy:
    cargo run -p treer-proxy

web:
    cd web && pnpm dev

web-build:
    cd web && pnpm build

agent-server *args:
    cargo run -p treer-agent-server -- {{args}}

stage-artifacts:
    sh scripts/stage-local-artifacts.sh

deploy-canary:
    sh scripts/deploy-canary.sh

test-canary:
    sh scripts/test-canary.sh
