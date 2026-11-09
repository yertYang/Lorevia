# Independent native runtime. The shipped image has no Python interpreter.
# Local single-user deployment: publish the UI on 127.0.0.1 only.
FROM rust:1.89.0-slim-bookworm AS native-builder
WORKDIR /build
COPY Cargo.toml Cargo.lock VERSION ./
COPY rust/ ./rust/
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --release --locked -p deeptutor-server \
    && cp target/release/yanzhizhou target/release/deeptutor target/release/deeptutor-server /usr/local/bin/

FROM node:22.23.4-bookworm-slim AS lesson-builder
WORKDIR /build/video-lesson
COPY packages/video-lesson/ ./
RUN npm ci --omit=dev --no-audit --no-fund

FROM node:22.23.4-bookworm-slim AS web-builder
WORKDIR /build/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --legacy-peer-deps
COPY web/ ./
# Read as version metadata by Next.js; no Python execution during the build.
COPY VERSION /build/VERSION
RUN npm run build

FROM node:22.23.4-bookworm-slim AS production
ARG DEEPTUTOR_BUILD_REVISION=unknown
LABEL org.opencontainers.image.revision=$DEEPTUTOR_BUILD_REVISION
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=lesson-builder /build/video-lesson /app/video-lesson
ENV DEEPTUTOR_VIDEO_RUNTIME=/app/video-lesson
COPY --from=native-builder /usr/local/bin/yanzhizhou /usr/local/bin/yanzhizhou
COPY --from=native-builder /usr/local/bin/deeptutor /usr/local/bin/deeptutor
COPY --from=native-builder /usr/local/bin/deeptutor-server /usr/local/bin/deeptutor-server
COPY --chown=node:node --from=web-builder /build/web/.next/standalone ./web/
COPY --chown=node:node --from=web-builder /build/web/.next/static ./web/.next/static/
COPY --chown=node:node --from=web-builder /build/web/public ./web/public/
COPY LICENSE THIRD_PARTY_NOTICES.txt ./
RUN mkdir -p /app/data && chown node:node /app /app/data
USER node
ENV NODE_ENV=production DEEPTUTOR_HOME=/app NEXT_TELEMETRY_DISABLED=1
EXPOSE 3782
VOLUME ["/app/data"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s \
    CMD node -e "fetch('http://127.0.0.1:8002/health/ready').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"
ENTRYPOINT ["yanzhizhou"]
CMD ["start", "--web", "/app/web", "--frontend-host", "0.0.0.0"]
