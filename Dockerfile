# Linux AArch64 verification image. Build it with --platform=linux/arm64.
FROM rust:bookworm

RUN apt-get update \
    && apt-get install --yes --no-install-recommends build-essential make python3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# The repository pins the host toolchain in rust-toolchain.toml; install it
# explicitly here so Docker does not rely on an interactive rustup prompt.
RUN rustup toolchain install nightly-2026-07-24 \
        --profile minimal \
        --component rust-src \
        --component llvm-tools-preview

# Keep the container's verification contract identical to the host's.
RUN make test
