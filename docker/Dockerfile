# Manual test rig for bergr: tmux + claude + bergr in one container.
# Not a build artifact for release — local verification only.

FROM rust:1-slim-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    tmux curl ca-certificates gnupg locales \
    && curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && npm install -g @anthropic-ai/claude-code \
    && apt-get purge -y gnupg curl \
    && rm -rf /var/lib/apt/lists/* \
    && sed -i '/en_US.UTF-8/s/^# //' /etc/locale.gen && locale-gen

ENV LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8

COPY --from=builder /src/target/release/bergr /usr/local/bin/bergr
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
COPY docker/fake-event.sh /usr/local/bin/fake-event
RUN chmod +x /usr/local/bin/entrypoint.sh /usr/local/bin/fake-event

ENV HOME=/root
WORKDIR /work

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["tmux", "new", "-s", "test"]
