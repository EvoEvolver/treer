# syntax=docker/dockerfile:1
FROM mcr.microsoft.com/devcontainers/rust:2-1-bookworm

ARG TARGETARCH
ARG NODE_VERSION=24.19.0
ARG PNPM_VERSION=11.22.0
ARG CODEX_VERSION=0.148.0
ARG JUST_VERSION=1.58.0

USER root
SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates \
        clang \
        cmake \
        curl \
        gh \
        git \
        git-lfs \
        jq \
        libssl-dev \
        openssh-client \
        pkg-config \
        protobuf-compiler \
        python3 \
        ripgrep \
        tmux \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

RUN case "${TARGETARCH:-amd64}" in \
        amd64) node_arch=x64 ;; \
        arm64) node_arch=arm64 ;; \
        *) echo "unsupported architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && archive="node-v${NODE_VERSION}-linux-${node_arch}.tar.xz" \
    && cd /tmp \
    && curl -fsSLO "https://nodejs.org/dist/v${NODE_VERSION}/${archive}" \
    && curl -fsSLO "https://nodejs.org/dist/v${NODE_VERSION}/SHASUMS256.txt" \
    && grep " ${archive}$" SHASUMS256.txt | sha256sum -c - \
    && tar -xJf "${archive}" --strip-components=1 -C /usr/local \
    && rm -f "${archive}" SHASUMS256.txt \
    && npm install --global \
        "@openai/codex@${CODEX_VERSION}" \
        "pnpm@${PNPM_VERSION}" \
    && npm cache clean --force \
    && cargo install just --locked --version "${JUST_VERSION}"

ENV HOME=/workspace \
    NPM_CONFIG_UPDATE_NOTIFIER=false \
    TREER_NETWORK_MODE=proxy-env

COPY entrypoint.sh /usr/local/bin/treer-canary-machine
RUN chmod 0755 /usr/local/bin/treer-canary-machine \
    && mkdir -p /workspace

WORKDIR /workspace/treer
ENTRYPOINT ["treer-canary-machine"]
