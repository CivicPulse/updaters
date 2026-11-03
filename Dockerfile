# syntax=docker/dockerfile:1
FROM rust:1.99.1-bookworm@sha256:9181b56c2a21ab1d5e73cffe04fc762426d30aa7fac3c91141dd033f49a7fc8b AS build
ARG TOOL
WORKDIR /workspace
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY tools ./tools
RUN case "$TOOL" in civpulse-download|civpulse-process|civpulse-upload|civpulse-results-collector) ;; *) exit 1 ;; esac \
    && cargo build --locked --release --package "$TOOL" \
    && mkdir /runtime \
    && cp "target/release/$TOOL" /runtime/civpulse-tool \
    && if [ "$TOOL" = civpulse-results-collector ]; then cargo build --locked --release --package civpulse-results-collector --example measure && cp target/release/examples/measure /runtime/civpulse-measure; fi

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:08476c244888bfb425bae1ae4dd7c17f093e972aab8426883d97f3c6f280f7c1 AS runtime
COPY --from=build /runtime/ /usr/local/bin/
USER 65532:65532
ENV RUST_LOG=info
ENTRYPOINT ["/usr/local/bin/civpulse-tool"]
