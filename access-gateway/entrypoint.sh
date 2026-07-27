#!/bin/sh
set -eu

source_dir=/var/run/labweaver-secrets-src
target_dir=/etc/labweaver/secrets

for name in ssh_host_ed25519_key mtls.crt mtls.key mtls-ca.pem target_key target_key-cert.pub; do
    test -s "${source_dir}/${name}"
done

install -m 0600 "${source_dir}/ssh_host_ed25519_key" "${target_dir}/ssh_host_ed25519_key"
install -m 0640 "${source_dir}/mtls.crt" "${target_dir}/mtls.crt"
install -m 0640 "${source_dir}/mtls.key" "${target_dir}/mtls.key"
install -m 0640 "${source_dir}/mtls-ca.pem" "${target_dir}/mtls-ca.pem"
chown root:gateway-auth "${target_dir}/mtls.crt" "${target_dir}/mtls.key" "${target_dir}/mtls-ca.pem"
install -o gateway -g gateway -m 0600 "${source_dir}/target_key" "${target_dir}/target_key"
install -o gateway -g gateway -m 0644 "${source_dir}/target_key-cert.pub" "${target_dir}/target_key-cert.pub"

for name in LABWEAVER_ACCESS_URL LABWEAVER_GATEWAY_IDENTITY LABWEAVER_MTLS_CERT LABWEAVER_MTLS_KEY LABWEAVER_MTLS_CA; do
    eval "value=\${${name}:-}"
    test -n "${value}"
    case "${value}" in
        *[!A-Za-z0-9_./:@-]*)
            echo "LW_GATEWAY_CONFIGURATION_INVALID: ${name}" >&2
            exit 1
            ;;
    esac
    printf 'export %s=%s\n' "${name}" "${value}"
done > "${target_dir}/gateway.env"
chown root:gateway-auth "${target_dir}/gateway.env"
chmod 0640 "${target_dir}/gateway.env"

exec /usr/sbin/sshd -D -e -f /etc/ssh/sshd_config
