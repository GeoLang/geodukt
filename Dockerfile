FROM rust:bookworm AS builder

# proj-sys vendors PROJ 9.6.2 and builds it with cmake, because bookworm ships
# 9.1.1 and the crate needs 9.6.2 or newer. PROJ links against rusqlite's bundled
# sqlite library, but its build still shells out to the sqlite3 binary to
# assemble proj.db, so that package is needed too.
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake g++ make sqlite3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY . .
RUN cargo build --release -p geodukt-cli

# PROJ links statically, but its CRS database and init files are data that stay
# behind in the build directory. Without them reproject and buffer cannot resolve
# an EPSG code at runtime. Matches the installed copy rather than the test
# fixture beside it, and fails loudly if a future base image links a system
# libproj, since the runtime stage would then need that library instead of this.
RUN set -eux; \
    db="$(find target/release/build -path '*/out/share/proj/proj.db' | head -1)"; \
    cp -r "$(dirname "$db")" /tmp/proj-data

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -r -s /bin/false geodukt

COPY --from=builder /app/target/release/geodukt /usr/local/bin/geodukt
COPY --from=builder /tmp/proj-data /usr/share/proj

USER geodukt

ENV RUST_LOG=info,geodukt=debug
ENV PROJ_DATA=/usr/share/proj

EXPOSE 8100

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8100/health || exit 1

ENTRYPOINT ["geodukt"]
CMD ["serve", "--bind", "0.0.0.0:8100"]
