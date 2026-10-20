# syntax=docker/dockerfile:1
# rustypdf: multi-stage build (cargo-chef caches dependencies), distroless
# runtime as nonroot, fonts baked in. Build: `just docker`.

# The Rust version matches rust-toolchain.toml. RUSTUP_TOOLCHAIN makes rustup
# use the image's toolchain instead of reading rust-toolchain.toml (which
# would download rustfmt/clippy, not needed for a release build).
ARG RUST_IMAGE=rust:1.97.0-bookworm

FROM ${RUST_IMAGE} AS chef
ENV RUSTUP_TOOLCHAIN=1.97.0 CARGO_TERM_COLOR=never
RUN cargo install cargo-chef --locked --version 0.1.77
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --bin rustypdf

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /app/target/release/rustypdf /rustypdf
# Noto Sans Bengali + Inter, all weights (OFL), so output is identical
# everywhere. Extra fonts can be mounted under /fonts/extra.
COPY fonts /fonts
# Default templates; mount your own folder over /templates.
COPY templates /templates
ENV TEMPLATES_DIR=/templates FONTS_DIR=/fonts PORT=8080 LOG_FORMAT=json
EXPOSE 8080
USER nonroot
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s CMD ["/rustypdf", "healthcheck"]
ENTRYPOINT ["/rustypdf"]
