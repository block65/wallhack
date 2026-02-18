FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    curl \
    ca-certificates \
    gcc \
    git \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none

ENV PATH="/root/.cargo/bin:${PATH}"

RUN rustup toolchain install nightly --no-self-update
