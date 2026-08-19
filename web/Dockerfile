FROM node:22-bookworm-slim AS builder

RUN corepack enable
WORKDIR /app
COPY package.json pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY . ./
RUN pnpm build

FROM nginx:1.27-alpine

COPY --from=builder /app/dist /usr/share/nginx/html
COPY nginx.conf.template /etc/nginx/templates/default.conf.template

ENV PORT=8080
ENV TREER_PROXY_PUBLIC_URL=http://127.0.0.1:8787/
EXPOSE 8080
