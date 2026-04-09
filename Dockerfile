FROM rust:1.87-bookworm AS builder

# Install libclang for pam-sys bindgen
RUN apt-get update && apt-get install -y --no-install-recommends \
    libclang-dev libpam0g-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Use Docker-specific Cargo.toml (no local path dependencies)
COPY docker/Cargo.toml ./Cargo.toml
COPY src ./src

RUN cargo build --release

FROM debian:bookworm-slim

# Install PAM and create test user
RUN apt-get update && apt-get install -y --no-install-recommends \
    libpam0g \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -s /bin/bash testuser \
    && echo "testuser:testpass" | chpasswd \
    && useradd -m -s /bin/bash greeter

# Copy binaries
COPY --from=builder /build/target/release/greetd /usr/bin/greetd
COPY --from=builder /build/target/release/test-greeter /usr/bin/test-greeter

# PAM configuration
COPY docker/pam.d/greetd /etc/pam.d/greetd
COPY docker/pam.d/greetd-greeter /etc/pam.d/greetd-greeter

# greetd configuration
COPY docker/config.toml /etc/greetd/config.toml

# Create runtime directory
RUN mkdir -p /run

CMD ["/usr/bin/greetd", "/etc/greetd/config.toml"]
