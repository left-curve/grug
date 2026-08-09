FROM debian:bookworm-slim

LABEL org.opencontainers.image.description="Dango archive docker image"
LABEL org.opencontainers.image.revision=$GIT_COMMIT
LABEL org.opencontainers.image.source="https://github.com/left-curve/left-curve"

ARG TARGET_ARCH
ARG GIT_COMMIT
ENV GIT_COMMIT=$GIT_COMMIT

RUN apt-get update && \
    apt-get upgrade -y && \
    apt-get install -y libssl-dev pkg-config libsqlite3-0 ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*

# The binary is cross-compiled outside the image (same as dango) and copied in.
# `cross build --release` already produces it as part of the workspace build.
COPY target/${TARGET_ARCH}/release/archive /usr/local/bin/

WORKDIR /app

# Expose the read-API httpd (8080) and the Prometheus metrics endpoint (9191).
EXPOSE 8080 9191

# Run the archive.
# This assumes the `app.toml` file has been mounted to
# `/root/.archive/config/app.toml` (the default home), or that the
# config is supplied via `--home` / `SECTION__FIELD` environment overrides.
CMD ["archive", "start"]
