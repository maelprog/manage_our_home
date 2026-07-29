# Minimal build image for the whole Cargo workspace (apps/api depends on
# apps/shared as a path dependency, so the workspace root — not just
# apps/api/ — needs to be in the build context). See
# infra/docker-compose.yml, which sets `context: ..` and
# `dockerfile: apps/api/Dockerfile` for this reason.
FROM rust:1-slim-bookworm AS builder
WORKDIR /workspace
COPY . .
RUN cargo build --release -p manage_our_home

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /workspace/target/release/manage_our_home /usr/local/bin/manage_our_home
# The orphaned-attachment reconciliation pass (#58) ships alongside the
# server so it can be run against a deployed stack — `docker compose run
# --rm api reconcile-attachments` — rather than needing a Rust toolchain
# next to production. It is a manual pass, not part of CMD.
COPY --from=builder /workspace/target/release/reconcile-attachments /usr/local/bin/reconcile-attachments
COPY --from=builder /workspace/apps/api/migrations /migrations
EXPOSE 8080
CMD ["manage_our_home"]
