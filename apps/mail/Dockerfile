FROM node:22-alpine AS web
RUN corepack enable
WORKDIR /src/apps/mail/web
COPY apps/mail/web/package.json apps/mail/web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY apps/mail/web/ ./
RUN pnpm build

FROM rust:1.88-bookworm AS server
WORKDIR /src
COPY . .
RUN cargo build --release -p treer-mail

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=server /src/target/release/treer-mail /usr/local/bin/treer-mail
COPY --from=web /src/apps/mail/web/dist /app/web
ENV TREER_MAIL_LISTEN=0.0.0.0:8788 TREER_MAIL_WEB_DIR=/app/web
EXPOSE 8788
ENTRYPOINT ["treer-mail"]
