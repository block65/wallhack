FROM alpine:latest

RUN apk add --no-cache samba-common-tools samba && \
  mkdir -p /shared

COPY <<EOF /etc/samba/smb.conf
[global]
workgroup = WORKGROUP
security = user
map to guest = Bad User

[shared]
path = /shared
browseable = yes
read only = no
guest ok = no
EOF

CMD sh -c 'adduser -D $SAMBA_USER && (echo $SAMBA_PASSWORD; echo $SAMBA_PASSWORD) | smbpasswd -a -s $SAMBA_USER && smbd -F --no-process-group'
