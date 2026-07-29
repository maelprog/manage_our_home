# Same "whole workspace as build context" reasoning as apps/api/Dockerfile
# — apps/web depends on apps/shared as a path dependency.
FROM rust:1-slim-bookworm AS builder
WORKDIR /workspace
COPY . .
RUN cargo build --release -p manage_our_home_web

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /workspace/target/release/manage_our_home_web /usr/local/bin/manage_our_home_web
# The self-hosted fonts (#67). The runtime image has no repo checkout, so
# the default repo-relative path in src/assets.rs resolves to nothing here —
# copy the directory in and point the server at the copy. Without this the
# fonts 404 and the app silently falls back to Georgia and system-ui, which
# nothing reports.
COPY --from=builder /workspace/apps/web/assets /usr/local/share/manage_our_home_web/assets
ENV WEB_ASSETS_DIR=/usr/local/share/manage_our_home_web/assets
EXPOSE 3000
CMD ["manage_our_home_web"]
