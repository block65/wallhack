FROM debian:trixie-slim AS builder

RUN apt-get update -qq && \
    apt-get install -y -qq --no-install-recommends \
    ca-certificates curl \
    build-essential autoconf automake \
    libssl-dev && \
    rm -rf /var/lib/apt/lists/*

ARG SOCAT_VERSION=1.8.1.1

WORKDIR /build/socat-${SOCAT_VERSION}
# socat author doesnt even have SSL, so we get it from the googlesource mirror
RUN curl -fsSL "https://third-party-mirror.googlesource.com/socat/+archive/refs/tags/tag-${SOCAT_VERSION}.tar.gz" | tar -xz
RUN autoconf && ./configure --disable-shared LDFLAGS="-static" && \
    make -j"$(nproc)" && \
    strip socat && \
    cp socat /build/socat

FROM scratch
COPY --from=builder /build/socat /bin/socat
