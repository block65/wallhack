FROM alpine:latest

# Squid forward proxy - supports HTTP CONNECT for WebSocket tunneling
RUN apk add --no-cache squid

# Simple permissive config for the range
RUN echo 'http_port 3128' > /etc/squid/squid.conf && \
    echo 'acl all src 0.0.0.0/0' >> /etc/squid/squid.conf && \
    echo 'acl SSL_ports port 443 4433 8080' >> /etc/squid/squid.conf && \
    echo 'acl CONNECT method CONNECT' >> /etc/squid/squid.conf && \
    echo 'http_access allow CONNECT SSL_ports' >> /etc/squid/squid.conf && \
    echo 'http_access allow all' >> /etc/squid/squid.conf && \
    echo 'access_log none' >> /etc/squid/squid.conf

EXPOSE 3128

CMD ["squid", "-N", "-f", "/etc/squid/squid.conf"]
