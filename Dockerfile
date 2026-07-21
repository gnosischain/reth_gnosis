FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

LABEL org.opencontainers.image.source=https://github.com/paradigmxyz/reth
LABEL org.opencontainers.image.licenses="MIT OR Apache-2.0"

# Install system dependencies.
# reth v2.4.0's default features pull in `jit` (revmc -> llvm-sys 221 -> LLVM 22)
# and `gmp` (gmp-mpfr-sys -> m4), so the builder needs LLVM 22 (+ Polly for linking)
# and m4. LLVM_SYS_221_PREFIX must be an ENV (not just .cargo/config.toml) because the
# `cargo chef cook` step compiles revmc/llvm-sys before the source tree is COPYed in.
RUN apt-get update && apt-get -y upgrade \
    && apt-get install -y libclang-dev pkg-config m4 wget gnupg \
    && . /etc/os-release \
    && wget -qO- https://apt.llvm.org/llvm-snapshot.gpg.key | gpg --dearmor -o /usr/share/keyrings/llvm-snapshot.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/llvm-snapshot.gpg] https://apt.llvm.org/${VERSION_CODENAME}/ llvm-toolchain-${VERSION_CODENAME}-22 main" > /etc/apt/sources.list.d/llvm-22.list \
    && apt-get update \
    && apt-get install -y llvm-22 llvm-22-dev libpolly-22-dev \
    && rm -rf /var/lib/apt/lists/*
ENV LLVM_SYS_221_PREFIX=/usr/lib/llvm-22

# Builds a cargo-chef plan
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json

# Build profile, release by default
ARG BUILD_PROFILE=release
ENV BUILD_PROFILE $BUILD_PROFILE

# Extra Cargo flags
ARG RUSTFLAGS=""
ENV RUSTFLAGS "$RUSTFLAGS"

# Extra Cargo features
ARG FEATURES=""
ENV FEATURES $FEATURES

# Builds dependencies
RUN cargo chef cook --profile $BUILD_PROFILE --features "$FEATURES" --recipe-path recipe.json

# Build application
COPY . .
RUN cargo build --profile $BUILD_PROFILE --features "$FEATURES" --locked --bin reth

# ARG is not resolved in COPY so we have to hack around it by copying the
# binary to a temporary location
RUN cp /app/target/$BUILD_PROFILE/reth /app/reth

# Use Ubuntu as the release image
FROM ubuntu AS runtime
WORKDIR /app

# Install CA certs and basic tools
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
 && rm -rf /var/lib/apt/lists/*

# Copy reth over from the build stage
COPY --from=builder /app/reth /usr/local/bin

COPY ./scripts/chainspecs ./chainspecs

EXPOSE 30303 30303/udp 9001 8545 8546
ENTRYPOINT ["/usr/local/bin/reth"]
