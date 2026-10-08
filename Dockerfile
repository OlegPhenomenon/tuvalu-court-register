# Tuvalu Court Register — single small image (~15 MB runtime).
# Build: docker build -t tuvalu-court .

FROM node:22.23.3-alpine AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

FROM rust:1.98.1-alpine AS server
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY --from=web /src/web/dist ./web/dist
RUN cargo build --release --locked && strip target/release/tuvalu-court

FROM alpine:3.22
RUN adduser -D -H -u 10001 court && mkdir -p /data && chown court /data
COPY --from=server /src/target/release/tuvalu-court /usr/local/bin/tuvalu-court
USER court
ENV TCR_DATA_DIR=/data TCR_BIND=0.0.0.0:8088
VOLUME ["/data"]
EXPOSE 8088
HEALTHCHECK --interval=30s --timeout=3s CMD wget -qO- http://127.0.0.1:8088/api/health || exit 1
ENTRYPOINT ["/usr/local/bin/tuvalu-court"]
CMD ["serve"]
