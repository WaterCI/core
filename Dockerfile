FROM debian:bullseye-slim AS runner
RUN apt update && \
    apt upgrade -yq && \
    #apt install -yq docker.io git && \
    rm -vrf /var/lib/apt/sources.list.d/

FROM rust:latest AS builder
WORKDIR /code
# Here, we will cache the dependencies
COPY Cargo.toml .
COPY src/_dummy.rs src/dummy.rs
RUN sed -i 's%src/main%src/dummy%' Cargo.toml && sed -i 's%lib/mod.rs%dummy.rs%' Cargo.toml && sed -i 's%lib.rs%dummy.rs%' Cargo.toml
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release
RUN rm Cargo.toml
# now, build as normal
COPY . /code
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release

FROM runner
WORKDIR /app
ENV SENTRY_ENVIRONMENT = development
ENV SENTRY_DSN = undefined
COPY --from=builder /code/target/release/waterci-core .
# always good practice to include the source code used to build the binary
COPY --from=builder /code/src src
#COPY --from=builder /code/resources src/resources
USER 1000
CMD ["./waterci-core"]