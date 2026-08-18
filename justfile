set shell := ["zsh", "-cu"]

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
