# syntax=docker/dockerfile:1
FROM rust:1.99.1-bookworm AS build
ARG TOOL
WORKDIR /workspace
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY tools ./tools
RUN case "$TOOL" in civpulse-download|civpulse-process|civpulse-upload) ;; *) exit 1 ;; esac \
    && cargo build --locked --release --package "$TOOL" \
    && cp "target/release/$TOOL" /usr/local/bin/civpulse-tool

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/civpulse-tool /usr/local/bin/civpulse-tool
USER 65532:65532
ENV RUST_LOG=info
ENTRYPOINT ["/usr/local/bin/civpulse-tool"]
