FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl jq python3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /opt/treer
COPY entrypoint.sh /usr/local/bin/treer-canary-machine
RUN chmod 755 /usr/local/bin/treer-canary-machine

ENV TREER_NETWORK_MODE=proxy-env
ENTRYPOINT ["treer-canary-machine"]
