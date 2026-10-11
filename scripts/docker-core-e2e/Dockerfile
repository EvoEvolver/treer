FROM debian:bookworm-slim
# Deliberately omit procps: the runner verifies connect rejects missing ps
# before consuming the enrollment key, then installs it and retries that key.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl python3 coreutils hostname openssl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/sh treertest
RUN openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj /CN=localhost \
    -addext subjectAltName=IP:127.0.0.1,DNS:localhost \
    -keyout /tmp/treer-e2e-key.pem -out /tmp/treer-e2e-cert.pem \
    && chmod 644 /tmp/treer-e2e-key.pem
USER treertest
ENV PATH=/home/treertest/.local/bin:/usr/local/bin:/usr/bin:/bin
WORKDIR /home/treertest
CMD ["sleep", "infinity"]
