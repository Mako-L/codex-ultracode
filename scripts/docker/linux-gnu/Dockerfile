FROM rust:bookworm
ARG RUST_TARGET=aarch64-unknown-linux-gnu
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 libcap-dev pkg-config libssl-dev ca-certificates binutils \
    && rustup toolchain install 1.95.0 --profile minimal --no-self-update \
    && rustup default 1.95.0 \
    && rustup target add "${RUST_TARGET}" \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_TOOLCHAIN=1.95.0
ENV CARGO_INCREMENTAL=0
ENV CARGO_PROFILE_RELEASE_DEBUG=0
ENV CARGO_PROFILE_RELEASE_STRIP=symbols
ENV RUSTFLAGS="-C link-arg=-fuse-ld=bfd"
