FROM rust:1.97-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN mkdir benches && echo 'fn main() {}' > benches/perf.rs \
    && mkdir -p tests/compat && echo 'fn main() {}' > tests/compat/main.rs \
    && cargo build --release --locked --bin raikou

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/raikou /usr/local/bin/raikou
EXPOSE 6379
USER nonroot
ENTRYPOINT ["/usr/local/bin/raikou"]
CMD ["--bind", "0.0.0.0", "--port", "6379"]
