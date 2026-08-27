FROM docker:27.5.1-cli
ARG TREER_VERSION=0.0.0
ARG TREER_BUILD_COMMIT=unknown
RUN apk add --no-cache python3
COPY updater.py /usr/local/bin/treer-updater.py
LABEL org.opencontainers.image.source="https://github.com/EvoEvolver/treer"
LABEL org.opencontainers.image.version="${TREER_VERSION}"
LABEL org.opencontainers.image.revision="${TREER_BUILD_COMMIT}"
EXPOSE 7420
ENTRYPOINT ["python3", "/usr/local/bin/treer-updater.py"]
