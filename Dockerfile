FROM rust:1.88-bookworm

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
