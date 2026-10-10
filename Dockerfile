# Build: tests and validation run first, so broken lessons or prompts fail the deploy
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
COPY data ./data
RUN cargo test --release --locked && cargo build --release --locked \
 && ./target/release/taal-teacher validate

# Run (TLS roots are compiled in, so no CA package is needed)
FROM debian:bookworm-slim
COPY --from=build /src/target/release/taal-teacher /usr/local/bin/taal-teacher
ENV PORT=8080 TEACHER_DB=/data/teacher.db
EXPOSE 8080
CMD ["taal-teacher"]
