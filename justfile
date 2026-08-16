set shell := ["zsh", "-cu"]

fmt:
    cargo fmt --all

test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets -- -D warnings

check:
    cargo fmt --all -- --check
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings

proxy:
    cargo run -p treer-proxy

agent-server *args:
    cargo run -p treer-agent-server -- {{args}}

stage-artifacts:
    sh scripts/stage-local-artifacts.sh
