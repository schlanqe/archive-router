FROM --platform=$BUILDPLATFORM rust:1.65.0 AS archive-router-builder
RUN apt-get update && apt-get install protobuf-compiler -y
WORKDIR /archive-router
COPY ./ .
RUN rm -r crates/network-scheduler
RUN rm -r crates/query-gateway
RUN cargo build --release

FROM --platform=$BUILDPLATFORM debian:bullseye-slim AS archive-router
RUN apt-get update && apt-get install ca-certificates -y
WORKDIR /archive-router
COPY --from=archive-router-builder /archive-router/target/release/router ./router
ENTRYPOINT ["/archive-router/router"]
EXPOSE 3000

FROM --platform=$BUILDPLATFORM lukemathwalker/cargo-chef:0.1.62-rust-1.74-bookworm AS chef
WORKDIR /app

FROM --platform=$BUILDPLATFORM chef AS network-planner

COPY Cargo.toml .
COPY Cargo.lock .
COPY crates ./crates

COPY subsquid-network/Cargo.toml ./subsquid-network/
COPY subsquid-network/Cargo.lock ./subsquid-network/
COPY subsquid-network/transport ./subsquid-network/transport

RUN cargo chef prepare --recipe-path recipe.json

FROM --platform=$BUILDPLATFORM chef AS network-builder

RUN --mount=target=/var/lib/apt/lists,type=cache,sharing=locked \
    --mount=target=/var/cache/apt,type=cache,sharing=locked \
    rm -f /etc/apt/apt.conf.d/docker-clean \
    && apt-get update \
    && apt-get -y install protobuf-compiler

COPY --from=network-planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY Cargo.toml .
COPY Cargo.lock .
COPY crates ./crates

COPY subsquid-network/Cargo.toml ./subsquid-network/
COPY subsquid-network/Cargo.lock ./subsquid-network/
COPY subsquid-network/transport ./subsquid-network/transport

RUN cargo build --release --workspace

FROM --platform=$BUILDPLATFORM debian:bookworm-slim as network-base

RUN --mount=target=/var/lib/apt/lists,type=cache,sharing=locked \
    --mount=target=/var/cache/apt,type=cache,sharing=locked \
    rm -f /etc/apt/apt.conf.d/docker-clean \
    && apt-get update \
    && apt-get -y install ca-certificates net-tools

FROM --platform=$BUILDPLATFORM network-base as network-scheduler

WORKDIR /run

COPY --from=network-builder /app/target/release/network-scheduler /usr/local/bin/network-scheduler
COPY --from=network-builder /app/crates/network-scheduler/config.yml .

ENV P2P_LISTEN_ADDR="/ip4/0.0.0.0/tcp/12345"
ENV HTTP_LISTEN_ADDR="0.0.0.0:8000"
ENV BOOTSTRAP="true"

CMD ["network-scheduler"]

RUN echo "PORT=\${HTTP_LISTEN_ADDR##*:}; netstat -an | grep \$PORT > /dev/null; if [ 0 != \$? ]; then exit 1; fi;" > ./healthcheck.sh
RUN chmod +x ./healthcheck.sh
HEALTHCHECK --interval=5s CMD ./healthcheck.sh

FROM --platform=$BUILDPLATFORM network-base as query-gateway
ARG TARGETOS
ARG TARGETARCH
ARG YQ_VERSION="4.40.5"

RUN --mount=target=/var/lib/apt/lists,type=cache,sharing=locked \
    --mount=target=/var/cache/apt,type=cache,sharing=locked \
    rm -f /etc/apt/apt.conf.d/docker-clean \
    && apt-get update \
    && apt-get -y install curl

RUN curl -sL https://github.com/mikefarah/yq/releases/download/v${YQ_VERSION}/yq_${TARGETOS}_${TARGETARCH} -o /usr/bin/yq \
    && chmod +x /usr/bin/yq

WORKDIR /run

COPY --from=network-builder /app/target/release/query-gateway /usr/local/bin/query-gateway
COPY --from=network-builder /app/crates/query-gateway/config.yml .

ENV P2P_LISTEN_ADDR="/ip4/0.0.0.0/tcp/12345"
ENV HTTP_LISTEN_ADDR="0.0.0.0:8000"
ENV BOOTSTRAP="true"
ENV PRIVATE_NODE="true"
ENV CONFIG_PATH="/run/config.yml"

CMD ["query-gateway"]

RUN echo "PORT=\${HTTP_LISTEN_ADDR##*:}; \
    yq '.available_datasets.[] | key' \$CONFIG_PATH \
    | xargs -I % curl -s http://localhost:\$PORT/network/%/height > /dev/null  " > ./healthcheck.sh
RUN chmod +x ./healthcheck.sh
HEALTHCHECK --interval=5s CMD ./healthcheck.sh

FROM --platform=$BUILDPLATFORM network-base as logs-collector

COPY --from=network-builder /app/target/release/logs-collector /usr/local/bin/logs-collector

ENV P2P_LISTEN_ADDR="/ip4/0.0.0.0/tcp/12345"
ENV BOOTSTRAP="true"
ENV PRIVATE_NODE="true"

CMD ["logs-collector"]
