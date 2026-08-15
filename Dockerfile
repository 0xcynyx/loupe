FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
RUN cargo build --release

FROM alpine:3.20
RUN apk add --no-cache ca-certificates
COPY --from=build /src/target/release/loupe /usr/local/bin/loupe
ENV MILVUS_GUI_BIND=0.0.0.0
ENV MILVUS_GUI_PORT=3003
EXPOSE 3003
ENTRYPOINT ["/usr/local/bin/loupe"]
