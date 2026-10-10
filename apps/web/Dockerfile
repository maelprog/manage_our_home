# Same "whole workspace as build context" reasoning as apps/api/Dockerfile
# — apps/web depends on apps/shared as a path dependency.
# Base images pinned by digest (#430): a bare `rust:1-slim-bookworm` or
# `debian:bookworm-slim` moves with no commit, so nothing in the repository
# said which base a published image was built on. The tag stays readable in
# the diff; Renovate (renovate.json) opens a PR when the digest behind it
# is republished.
FROM rust:1-slim-bookworm@sha256:2c3a22f0a5533ea2dd5a16627bc841228151faa2d4de2644ac9987e4a2f1f2fa AS builder
WORKDIR /workspace
COPY . .
RUN cargo build --release -p manage_our_home_web

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
# `apt-get upgrade`: a pinned digest freezes the Debian packages it was
# built with, and Debian's security fixes land in bookworm-security
# before the image is republished — on 2026-10-10 the pinned base carried
# three fixable CRITICAL CVEs in perl-base, enough to fail the Trivy gate of
# .github/workflows/image-build.yml. Upgrading here takes them at build time;
# the cost is that two builds of one commit can differ by those fixes.
RUN apt-get update && apt-get upgrade -y --no-install-recommends \
    && apt-get install -y --no-install-recommends ca-certificates \
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
# Unprivileged, and fine with a read-only root filesystem (#430): the
# server writes nothing to disk (it only reads its assets above), so it
# runs as a numeric UID — numeric so that Kubernetes' runAsNonRoot can
# check it without an /etc/passwd entry. The files copied above stay
# root-owned and not writable by this user.
USER 10001:10001
CMD ["manage_our_home_web"]
