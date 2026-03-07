FROM alpine:3.21 AS builder

RUN apk add --no-cache build-base bison flex libmnl-dev libmnl-static

ARG IPROUTE2_VERSION

WORKDIR /build
RUN wget -q "https://mirrors.kernel.org/pub/linux/utils/net/iproute2/iproute2-${IPROUTE2_VERSION}.tar.xz" && \
    tar xf "iproute2-${IPROUTE2_VERSION}.tar.xz"

WORKDIR /build/iproute2-${IPROUTE2_VERSION}

RUN ./configure

# Inject static netem lookup inside get_qdisc_kind (musl static can't dlsym)
RUN sed -i '/get_qdisc_kind(/,/{/s/{/{\n\textern struct qdisc_util netem_qdisc_util;\n\tif (strcmp(str, "netem") == 0) return \&netem_qdisc_util;\n/' tc/tc.c

# Verify the patch landed
RUN grep -A5 'get_qdisc_kind' tc/tc.c | head -10

RUN make -j"$(nproc)" \
    LDFLAGS="-static" \
    EXTRA_CFLAGS="-include endian.h -D_GNU_SOURCE" \
    SUBDIRS="lib tc"

# The moment of truth
RUN tc/tc qdisc add netem help 2>&1 | grep -q "delay" && \
    echo "SUCCESS: netem is operational"

RUN strip tc/tc && cp tc/tc /build/tc

FROM scratch
COPY --from=builder /build/tc /bin/tc
