FROM rust:1-bookworm AS builder
ARG TREER_BUILD_COMMIT=unknown
ENV TREER_BUILD_COMMIT=$TREER_BUILD_COMMIT

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY apps/mail/server ./apps/mail/server
COPY skills ./skills
RUN set -eu; \
    cargo build --locked --release -p treer-proxy -p treer-agent-host -p treer-agent-server -p treer-cli; \
    case "$(uname -m)" in \
      x86_64|amd64) platform=linux-x86_64 ;; \
      aarch64|arm64) platform=linux-aarch64 ;; \
      *) echo "unsupported build architecture $(uname -m)" >&2; exit 1 ;; \
    esac; \
    mkdir -p /out/bin; \
    mkdir -p "/out/dist/$platform"; \
    cp target/release/treer-proxy /out/bin/treer-proxy; \
    cp target/release/treer "/out/dist/$platform/treer"; \
    cp target/release/treer-agent-host "/out/dist/$platform/treer-agent-host"; \
    cp target/release/treer-agent-server "/out/dist/$platform/treer-agent-server"

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /out/bin/treer-proxy /usr/local/bin/treer-proxy
COPY --from=builder /out/dist /app/dist

ENV TREER_ARTIFACTS_DIR=/app/dist
EXPOSE 8080

ENTRYPOINT ["treer-proxy"]
