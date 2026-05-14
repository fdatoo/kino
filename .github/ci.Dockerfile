FROM ubuntu:24.04

LABEL org.opencontainers.image.source="https://github.com/fdatoo/kino"
LABEL org.opencontainers.image.description="Kino CI toolchain"

ENV DEBIAN_FRONTEND=noninteractive
ENV RUSTUP_HOME=/usr/local/rustup
ENV CARGO_HOME=/usr/local/cargo
ENV PATH=/usr/local/cargo/bin:$PATH

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        build-essential \
        ca-certificates \
        curl \
        ffmpeg \
        git \
        just \
        libsqlite3-dev \
        pkg-config \
        tesseract-ocr \
        unzip \
        xz-utils \
        zstd \
    && rm -rf /var/lib/apt/lists/*

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN curl --proto "=https" --tlsv1.2 -fsSL https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain stable \
    && rustup component add clippy rustfmt

WORKDIR /workspace
