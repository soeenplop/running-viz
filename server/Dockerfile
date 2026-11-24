FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 app
COPY --from=build /src/target/release/running-viz /usr/local/bin/running-viz
USER app
ENV PORT=8000
EXPOSE 8000
CMD ["running-viz"]
