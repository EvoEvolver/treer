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
    node --test scripts/release-r2.test.mjs
    cd web && pnpm typecheck
    cd web && pnpm build
    cd apps/mail/web && pnpm typecheck
    cd apps/mail/web && pnpm build
    python3 -m unittest discover -s apps/mail/tests -p 'test_*.py' -v
    python3 -m unittest discover -s apps/telegram/tests -p 'test_*.py' -v
    node --test apps/pi-ui/*.test.mjs
    cargo build --workspace
    cargo fmt --all -- --check
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings

proxy:
    cargo run -p treer-proxy

web:
    cd web && pnpm dev

web-build:
    cd web && pnpm build

mail config state=".treer/apps/mail":
    TREER_APP_CONFIG={{config}} TREER_APP_STATE_DIR={{state}} python3 apps/mail/mail.py

mail-web:
    cd apps/mail/web && pnpm dev

mail-test:
    python3 -m unittest discover -s apps/mail/tests -p 'test_*.py' -v

telegram-test:
    python3 -m unittest discover -s apps/telegram/tests -p 'test_*.py' -v

app-test:
    python3 -m unittest discover -s apps/mail/tests -p 'test_*.py' -v
    python3 -m unittest discover -s apps/telegram/tests -p 'test_*.py' -v
    node --test apps/pi-ui/*.test.mjs

messaging-e2e:
    cargo test -p treer-proxy message_
    just app-test

agent-server *args:
    cargo run -p treer-agent-server -- {{args}}

stage-artifacts:
    sh scripts/stage-local-artifacts.sh

collect-artifacts revision="HEAD":
    sh scripts/collect-build-artifacts.sh {{revision}}

test-canary:
    sh scripts/test-canary.sh

test-canary-provision:
    TREER_CANARY_PROVISION_MACHINES=1 sh scripts/test-canary.sh

test-canary-enroll:
    TREER_CANARY_PROVISION_MACHINES=1 TREER_CANARY_ENROLL_MACHINES=1 sh scripts/test-canary.sh

release-canary revision="HEAD":
    sh scripts/release-canary.sh {{revision}}

promote-production manifest:
    sh scripts/promote-production.sh {{manifest}}

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

artifacts-test:
    node --test scripts/release-r2.test.mjs
