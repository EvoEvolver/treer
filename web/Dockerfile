FROM node:22-bookworm AS build
WORKDIR /app
RUN corepack enable
COPY package.json pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY . .
RUN pnpm build

FROM nginx:1.27-alpine
ARG TREER_VERSION=0.0.0
ARG TREER_BUILD_COMMIT=unknown
COPY --from=build /app/dist /usr/share/nginx/html
COPY default.conf.template /etc/nginx/templates/default.conf.template
LABEL org.opencontainers.image.source="https://github.com/EvoEvolver/treer"
LABEL org.opencontainers.image.version="${TREER_VERSION}"
LABEL org.opencontainers.image.revision="${TREER_BUILD_COMMIT}"
ENV PORT=8080
ENV NGINX_ENVSUBST_FILTER="^(PORT|TREER_PROXY_PUBLIC_URL)$"
EXPOSE 8080
