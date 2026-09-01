FROM lukemathwalker/cargo-chef:latest-rust-1.97.1-trixie AS chef
WORKDIR /app

LABEL org.opencontainers.image.source=https://github.com/paradigmxyz/reth
LABEL org.opencontainers.image.licenses="MIT OR Apache-2.0"

# Install system dependencies.
# reth v2.4.0's default features pull in `jit` (revmc -> llvm-sys 221 -> LLVM 22) and
# `gmp` (gmp-mpfr-sys -> m4). LLVM setup mirrors reth's own CI via the vendored script,
# which symlinks `llvm-config` onto PATH so llvm-sys finds it (no LLVM_SYS_221_PREFIX
# needed). The script is COPYed in the `chef` base stage so it is present before
# `cargo chef cook` compiles revmc/llvm-sys.
COPY .github/scripts/install_llvm_ubuntu.sh /tmp/install_llvm_ubuntu.sh
RUN apt-get update && apt-get -y upgrade \
    && apt-get install -y libclang-dev pkg-config m4 \
    && bash /tmp/install_llvm_ubuntu.sh 22 \
    && rm -rf /var/lib/apt/lists/*

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
ENV RUSTFLAGS="$RUSTFLAGS"

# Extra Cargo features
ARG FEATURES=""
ENV FEATURES="$FEATURES"

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
