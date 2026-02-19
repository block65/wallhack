FROM alpine:latest AS builder

RUN apk add --no-cache gcc musl-dev make git
RUN git clone --depth=1 https://github.com/rofl0r/microsocks.git /src
RUN cd /src && make

FROM alpine:latest

COPY --from=builder /src/microsocks /usr/bin/microsocks

EXPOSE 1080

CMD ["microsocks", "-i", "0.0.0.0", "-p", "1080"]
