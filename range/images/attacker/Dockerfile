FROM alpine:latest

RUN apk add --no-cache openssh-client sshpass curl nmap netcat-openbsd socat bash

COPY target/musl/x86_64-unknown-linux-musl/release/wallhack /usr/bin/wallhack

CMD ["tail", "-f", "/dev/null"]
