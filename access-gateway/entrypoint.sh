#!/bin/sh
set -eu

source_dir=/var/run/labweaver-secrets-src
target_dir=/etc/labweaver/secrets

for name in ssh_host_ed25519_key mtls.crt mtls.key mtls-ca.pem target_key; do
    test -s "${source_dir}/${name}"
done

install -m 0600 "${source_dir}/ssh_host_ed25519_key" "${target_dir}/ssh_host_ed25519_key"
install -m 0640 "${source_dir}/mtls.crt" "${target_dir}/mtls.crt"
install -m 0640 "${source_dir}/mtls.key" "${target_dir}/mtls.key"
install -m 0640 "${source_dir}/mtls-ca.pem" "${target_dir}/mtls-ca.pem"
chown root:gateway-auth "${target_dir}/mtls.crt" "${target_dir}/mtls.key" "${target_dir}/mtls-ca.pem"
install -o gateway -g gateway -m 0600 "${source_dir}/target_key" "${target_dir}/target_key"

exec /usr/sbin/sshd -D -e -f /etc/ssh/sshd_config
