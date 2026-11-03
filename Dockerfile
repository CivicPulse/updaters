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

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:08476c244888bfb425bae1ae4dd7c17f093e972aab8426883d97f3c6f280f7c1 AS runtime
COPY --from=build /usr/local/bin/civpulse-tool /usr/local/bin/civpulse-tool
USER 65532:65532
ENV RUST_LOG=info
ENTRYPOINT ["/usr/local/bin/civpulse-tool"]
