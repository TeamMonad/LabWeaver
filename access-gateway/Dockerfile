FROM alpine:3.22

RUN apk add --no-cache openssh-client openssh-server \
    && adduser -D -H -s /sbin/nologin gateway-auth \
    && mkdir -p /run/sshd /etc/labweaver \
    && ssh-keygen -A

COPY target/release/access-gateway /usr/local/bin/labweaver-gateway
COPY access-gateway/sshd_config /etc/ssh/sshd_config
COPY access-gateway/target-ssh.conf /etc/labweaver/target-ssh.conf

EXPOSE 2222
ENTRYPOINT ["/usr/sbin/sshd", "-D", "-e", "-f", "/etc/ssh/sshd_config"]
