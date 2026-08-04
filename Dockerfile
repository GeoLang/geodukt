FROM rust:bookworm AS builder

WORKDIR /app
COPY . .
RUN cargo build --release -p geodukt-cli

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -r -s /bin/false geodukt

COPY --from=builder /app/target/release/geodukt /usr/local/bin/geodukt

USER geodukt

ENV RUST_LOG=info,geodukt=debug

EXPOSE 8100

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8100/health || exit 1

ENTRYPOINT ["geodukt"]
CMD ["serve", "--bind", "0.0.0.0:8100"]
