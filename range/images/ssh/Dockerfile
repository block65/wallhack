FROM alpine:latest

RUN apk add --no-cache openssh-server && \
  ssh-keygen -A && \
  echo 'root:toor' | chpasswd && \
  sed -i 's/#PermitRootLogin.*/PermitRootLogin yes/' /etc/ssh/sshd_config && \
  sed -i 's/#PasswordAuthentication.*/PasswordAuthentication yes/' /etc/ssh/sshd_config && \
  mkdir -p /root && \
  echo 'Nice pivot.' > /root/flag.txt

EXPOSE 22

CMD ["/usr/sbin/sshd", "-D", "-e"]
