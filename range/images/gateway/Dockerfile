FROM alpine:latest

RUN apk add --no-cache openssh socat netcat-openbsd curl iptables && \
  echo 'root:hacker' | chpasswd && \
  sed -i 's/#PermitRootLogin.*/PermitRootLogin yes/' /etc/ssh/sshd_config && \
  sed -i 's/#PasswordAuthentication.*/PasswordAuthentication yes/' /etc/ssh/sshd_config && \
  echo "PasswordAuthentication yes" >> /etc/ssh/sshd_config && \
  echo "PermitEmptyPasswords no" >> /etc/ssh/sshd_config

COPY target/musl/x86_64-unknown-linux-musl/release/wallhack /usr/bin/wallhack
COPY range/assets/ssh_keys/etc/ssh/ssh_host_* /etc/ssh/
RUN chmod 600 /etc/ssh/ssh_host_*_key

CMD ["/usr/sbin/sshd", "-D"]
