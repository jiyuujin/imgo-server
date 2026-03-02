# FROM rust:latest as builder

# WORKDIR /app

# COPY . .

# RUN apt-get update && apt-get install -y pkg-config && rm -rf /var/lib/apt/lists/*

# RUN cargo build --release

# FROM debian:bookworm-slim

# RUN apt-get update && apt-get install -y ca-certificates fonts-noto-cjk && rm -rf /var/lib/apt/lists/*

# COPY --from=builder /app/target/release/imgo-server /usr/local/bin/app

# CMD ["app"]

FROM rust:latest as builder

WORKDIR /app

COPY . .

RUN apt-get update && apt-get install -y \
    pkg-config \
    libfontconfig1-dev \
    && rm -rf /var/lib/apt/lists/*

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    fonts-noto-cjk \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/imgo-server /usr/local/bin/app

ENV FONT_REGULAR_PATH=/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc

ENV FONT_BOLD_PATH=/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc

CMD ["app"]
