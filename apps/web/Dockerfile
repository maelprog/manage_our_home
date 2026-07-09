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
EXPOSE 3000
CMD ["manage_our_home_web"]
