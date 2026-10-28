set shell := ["zsh", "-cu"]

test-db-up:
    if docker inspect treer-postgres-test >/dev/null 2>&1; then docker start treer-postgres-test >/dev/null; else docker run --name treer-postgres-test --shm-size=1g -e POSTGRES_PASSWORD=treer -e POSTGRES_USER=treer -e POSTGRES_DB=treer_test -p 127.0.0.1:55432:5432 -d postgres:17-alpine >/dev/null; fi

test-db-down:
    docker rm -f treer-postgres-test

fmt:
    cargo fmt --all

test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets -- -D warnings

service-canary:
    cargo test -p treer-agent-server service_canary_ -- --nocapture

# Everything CI runs, locally.
check:
    just scripts-test
    cd web && pnpm typecheck
    cd web && pnpm build
    cd web && pnpm test:e2e
    just updater-test
    just agent-launcher-test
    cargo build --workspace
    cargo fmt --all -- --check
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings

# Release, install-trust, deploy and docs tooling (fake registries and servers only).
scripts-test:
    node scripts/check-docs.mjs
    python3 -m unittest discover -s scripts/tests -p 'test_*.py' -v
    node --test scripts/*.test.mjs

proxy:
    cargo run -p treer-proxy

web:
    cd web && pnpm dev

web-build:
    cd web && pnpm build

web-test:
    cd web && pnpm test:e2e

mail config state=".treer/apps/mail":
    TREER_APP_CONFIG={{config}} TREER_APP_STATE_DIR={{state}} python3 apps/mail/mail.py

mail-web:
    cd apps/mail/web && pnpm dev

mail-test:
    python3 -m unittest discover -s apps/mail/tests -p 'test_*.py' -v

gits-test:
    python3 -m unittest discover -s apps/gits/tests -p 'test_*.py' -v

telegram-test:
    python3 -m unittest discover -s apps/telegram/tests -p 'test_*.py' -v

soul-test:
    python3 -m unittest discover -s apps/soul/tests -p 'test_*.py' -v

updater-test:
    python3 -m unittest discover -s deploy/updater/tests -p 'test_*.py' -v

app-test:
    python3 -m unittest discover -s apps/tests -p 'test_*.py' -v
    python3 -m unittest discover -s apps/mail/tests -p 'test_*.py' -v
    python3 -m unittest discover -s apps/gits/tests -p 'test_*.py' -v
    python3 -m unittest discover -s apps/telegram/tests -p 'test_*.py' -v
    python3 -m unittest discover -s apps/soul/tests -p 'test_*.py' -v
    just agent-launcher-test

agent-launcher-test:
    npm --prefix launchers/agent ci --ignore-scripts
    npm --prefix launchers/agent run check
    npm --prefix launchers/agent test
    TREER_TEST_NODE="$(command -v node)" cargo test --locked --features fixture-harness --manifest-path launchers/agent/Cargo.toml

messaging-e2e:
    cargo test -p treer-proxy message_
    just app-test

agent-server *args:
    cargo run -p treer-agent-server -- {{args}}

# Build and stage this machine's native artifacts into dist/<platform>/ (local testing only).
stage-artifacts:
    #!/bin/sh
    set -eu
    case "$(uname -s)-$(uname -m)" in
      Linux-x86_64|Linux-amd64) platform=linux-x86_64 ;;
      Linux-aarch64|Linux-arm64) platform=linux-aarch64 ;;
      Darwin-x86_64) platform=darwin-x86_64 ;;
      Darwin-arm64|Darwin-aarch64) platform=darwin-aarch64 ;;
      *) echo "unsupported platform $(uname -s)/$(uname -m)" >&2; exit 1 ;;
    esac
    payload=$(mktemp -d "${TMPDIR:-/tmp}/treer-launcher-payload.XXXXXX")
    trap 'rm -rf "$payload"' EXIT
    commit=$(git rev-parse HEAD)
    npm --prefix launchers/agent ci --ignore-scripts
    node scripts/prepare-launcher-release.mjs --output "$payload" --source-commit "$commit"
    TREER_LAUNCHER_PAYLOAD_DIR="$payload" TREER_REQUIRE_LAUNCHER_BUNDLE=1 \
        cargo build --locked --release -p treer-agent-host -p treer-agent-server -p treer-cli
    TREER_BUILD_COMMIT="$commit" sh scripts/stage-release-artifacts.sh "$platform" "dist/$platform"

# Verify dist/, or download (dispatching if needed) the native workflow artifacts for a commit.
fetch-artifacts revision="HEAD":
    sh scripts/fetch-machine-artifacts.sh {{revision}}

# Deploy the checked-out commit to the hosted service: canary first, then production.
deploy target:
    sh scripts/deploy-hosted.sh {{target}}

# Two-machine end-to-end test against the deployed Canary.
test-canary:
    sh scripts/test-canary.sh

test-canary-provision:
    TREER_CANARY_PROVISION_MACHINES=1 sh scripts/test-canary.sh

test-canary-enroll:
    TREER_CANARY_PROVISION_MACHINES=1 TREER_CANARY_ENROLL_MACHINES=1 sh scripts/test-canary.sh

web-worker-dev:
    cd web && pnpm worker:dev

artifacts-keygen:
    node scripts/release-r2.mjs keygen

artifacts-prepare version:
    node scripts/release-r2.mjs prepare --version "{{version}}"

artifacts-canary version:
    node scripts/release-r2.mjs publish --version "{{version}}" --channel canary

artifacts-stable version:
    node scripts/release-r2.mjs promote --version "{{version}}" --from-channel canary --channel stable

artifacts-verify version:
    node scripts/release-r2.mjs verify --version "{{version}}"

# Self-hosted single-container image: local source build plus disposable-container health.
single-image-smoke:
    python3 -I deploy/single-container/tests/build-smoke.py
