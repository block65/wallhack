FROM alpine:latest

RUN apk add --no-cache openssh socat netcat-openbsd curl nmap-ncat iptables && \
  echo 'root:hacker' | chpasswd && \
  sed -i 's/#PermitRootLogin.*/PermitRootLogin yes/' /etc/ssh/sshd_config && \
  sed -i 's/#PasswordAuthentication.*/PasswordAuthentication yes/' /etc/ssh/sshd_config && \
  echo "PasswordAuthentication yes" >> /etc/ssh/sshd_config

COPY assets/ssh_keys/etc/ssh/ssh_host_* /etc/ssh/
COPY images/gateway-hardened/hardened.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

EXPOSE 22

CMD ["/entrypoint.sh"]
