# Base image pinned by digest (supply chain): bump tag + digest together,
# e.g. `docker buildx imagetools inspect rust:1.88-bookworm`.
FROM rust:1.88-bookworm@sha256:af306cfa71d987911a781c37b59d7d67d934f49684058f96cf72079c3626bfe0

RUN rustup component add clippy

RUN apt-get update && apt-get install -y \
    libseccomp-dev \
    nodejs \
    npm \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace

# For testing inside Docker, we need privileges
# Run with: docker run --privileged --security-opt seccomp=unconfined

CMD ["bash"]
