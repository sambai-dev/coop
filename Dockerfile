FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p coop-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 nodejs bash ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/coop /usr/local/bin/coop
ENV COOP_ADDR=0.0.0.0:7300
ENV COOP_DB=/data/coop.db
EXPOSE 7300
VOLUME /data
ENTRYPOINT ["coop"]
