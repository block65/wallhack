FROM alpine:latest

RUN apk add --no-cache vsftpd && \
  echo 'anonymous_enable=YES' > /etc/vsftpd/vsftpd.conf && \
  echo 'local_enable=NO' >> /etc/vsftpd/vsftpd.conf && \
  echo 'write_enable=NO' >> /etc/vsftpd/vsftpd.conf && \
  echo 'anon_root=/ftp' >> /etc/vsftpd/vsftpd.conf && \
  mkdir -p /ftp && \
  echo 'FTP server - try anonymous login' > /ftp/readme.txt

EXPOSE 21

CMD ["vsftpd", "/etc/vsftpd/vsftpd.conf"]
