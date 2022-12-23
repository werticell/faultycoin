FROM rust:latest

COPY ./ ./

RUN cargo build --release

CMD ["/bin/bash", "-c", "LOG_LEVEL=INFO; ./target/release/faultycoin --config config/example.yaml"]