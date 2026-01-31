FROM alpine:latest

RUN apk add --no-cache openssh rsync tar && \
  echo 'root:hacker' | chpasswd && \
  adduser -D backup && \
  echo 'backup:backup123' | chpasswd && \
  sed -i 's/#PermitRootLogin.*/PermitRootLogin yes/' /etc/ssh/sshd_config && \
  sed -i 's/#PasswordAuthentication.*/PasswordAuthentication yes/' /etc/ssh/sshd_config && \
  echo "PasswordAuthentication yes" >> /etc/ssh/sshd_config

COPY assets/ssh_keys/etc/ssh/ssh_host_* /etc/ssh/
CMD ["/usr/sbin/sshd", "-D"]
