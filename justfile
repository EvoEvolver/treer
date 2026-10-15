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

service-canary:
    cargo test -p treer-agent-server service_canary_ -- --nocapture

check:
    node scripts/check-docs.mjs
    python3 -m unittest discover -s scripts/tests -p 'test_*.py' -v
    node --test scripts/release-r2.test.mjs scripts/machine-artifacts.test.mjs scripts/release-security.test.mjs scripts/prepare-launcher-release.test.mjs
    node --test scripts/install-path.test.mjs scripts/install-checksums.test.mjs scripts/ais-e2e.test.mjs
    cd web && pnpm typecheck
    cd web && pnpm build
    cd web && pnpm test:e2e
    python3 -m unittest discover -s deploy/updater/tests -p 'test_*.py' -v
    just agent-launcher-test
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
    npm --prefix launchers/agent ci
    npm --prefix launchers/agent run check
    npm --prefix launchers/agent test
    npm --prefix launchers/agent run test:browser

# Compatibility recipe name; the duplicate ACP runtime is retired.
acp-launcher-test:
    just agent-launcher-test

ais-e2e:
    node scripts/ais-e2e.mjs

messaging-e2e:
    cargo test -p treer-proxy message_
    just app-test

# Closed fake machines only; this never invokes an enrolled service.
machine-update-test:
    python3 -m unittest discover -s scripts/tests -p 'test_update_machine.py' -v

release-tooling-test:
    python3 -m unittest discover -s scripts/tests -p 'test_*.py' -v
    node --test scripts/ais-e2e.test.mjs

agent-server *args:
    cargo run -p treer-agent-server -- {{args}}

stage-artifacts:
    sh scripts/stage-local-artifacts.sh

collect-artifacts revision="HEAD":
    sh scripts/collect-build-artifacts.sh {{revision}}

ensure-artifacts revision="HEAD":
    sh scripts/ensure-machine-artifacts.sh {{revision}}

machine-artifacts-test:
    node --test scripts/machine-artifacts.test.mjs

test-canary:
    sh scripts/test-canary.sh

test-canary-provision:
    TREER_CANARY_PROVISION_MACHINES=1 sh scripts/test-canary.sh

test-canary-enroll:
    TREER_CANARY_PROVISION_MACHINES=1 TREER_CANARY_ENROLL_MACHINES=1 sh scripts/test-canary.sh

release-canary revision="HEAD":
    sh scripts/release-canary.sh {{revision}}

canary *args:
    python3 scripts/canary-release.py {{args}}

# Plan the gates for a candidate from its changed paths; runs nothing.
release-grade *args:
    python3 scripts/release-grade.py {{args}}

# Print (or with --create, create) the Issues ticket chain for a release.
release-tickets *args:
    python3 scripts/release-tickets.py {{args}}

canary-scripts-test:
    python3 -m unittest discover -s scripts/tests -p 'test_canary_release.py' -v

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

mobile-bundle-ui:
    bash scripts/bundle-mobile-agent-ui.sh

mobile-ios-ci:
    export DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer
    export PATH="$DEVELOPER_DIR/usr/bin:/opt/homebrew/bin:$PATH"
    cd mobile/ios && xcodegen generate
    xcodebuild -project mobile/ios/Treer.xcodeproj -scheme Treer -destination 'platform=iOS Simulator,name=iPhone 17 Pro,OS=27.0' -configuration Debug build CODE_SIGNING_ALLOWED=NO

mobile-android-ci:
    export JAVA_HOME=/opt/homebrew/opt/openjdk
    export ANDROID_HOME="$HOME/Library/Android/sdk"
    export ANDROID_SDK_ROOT="$ANDROID_HOME"
    cd mobile/android && ./gradlew :app:assembleDebug --no-daemon
