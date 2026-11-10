# syntax=docker/dockerfile:1
FROM rust:1.99.1-bookworm@sha256:9181b56c2a21ab1d5e73cffe04fc762426d30aa7fac3c91141dd033f49a7fc8b AS build
ARG TOOL
WORKDIR /workspace
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY tools ./tools
RUN case "$TOOL" in civpulse-download|civpulse-process|civpulse-upload|civpulse-results-collector|civpulse-webapi|civpulse-mcp) ;; *) exit 1 ;; esac \
    && cargo build --locked --release --package "$TOOL" \
    && mkdir /runtime \
    && cp "target/release/$TOOL" /runtime/civpulse-tool \
    && if [ "$TOOL" = civpulse-results-collector ]; then cargo build --locked --release --package civpulse-results-collector --example measure && cp target/release/examples/measure /runtime/civpulse-measure; fi

# Rust uses rustls, so OpenSSL and its compression dependencies are unnecessary.
# Retain libgcc for unwinding, along with its license and scanner inventory.
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:cc66d222f0a7ec996b78a99fe76a969bc80b539a5016ad866c14d1949bc6d0ab AS runtime-libs
FROM gcr.io/distroless/base-nossl-debian13:nonroot@sha256:e6b0f82595aa1edde72c8054de7a9dca30f0f8f54820c37a167c0ddfc21c5c67 AS runtime
COPY --from=runtime-libs /usr/lib/*-linux-gnu/libgcc_s.so.1 /lib/libgcc_s.so.1
COPY --from=runtime-libs /var/lib/dpkg/status.d/libgcc-s1 /var/lib/dpkg/status.d/libgcc-s1
COPY --from=runtime-libs /var/lib/dpkg/status.d/gcc-14-base /var/lib/dpkg/status.d/gcc-14-base
COPY --from=runtime-libs /usr/share/doc/gcc-14-base/ /usr/share/doc/gcc-14-base/
COPY --from=build /runtime/ /usr/local/bin/
USER 65532:65532
ENV RUST_LOG=info
ENTRYPOINT ["/usr/local/bin/civpulse-tool"]
