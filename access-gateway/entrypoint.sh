#!/bin/sh
set -eu

source_dir=/var/run/labweaver-secrets-src
target_dir=/etc/labweaver/secrets

for name in ssh_host_ed25519_key mtls-ca.pem service-oidc-ca.pem service-client-secret target_key target_key-cert.pub; do
    test -s "${source_dir}/${name}"
done

install -m 0600 "${source_dir}/ssh_host_ed25519_key" "${target_dir}/ssh_host_ed25519_key"
install -m 0640 "${source_dir}/mtls-ca.pem" "${target_dir}/mtls-ca.pem"
chown root:gateway-auth "${target_dir}/mtls-ca.pem"
install -m 0640 "${source_dir}/service-oidc-ca.pem" "${target_dir}/service-oidc-ca.pem"
install -m 0640 "${source_dir}/service-client-secret" "${target_dir}/service-client-secret"
chown root:gateway-auth "${target_dir}/service-oidc-ca.pem" "${target_dir}/service-client-secret"
install -o gateway -g gateway -m 0600 "${source_dir}/target_key" "${target_dir}/target_key"
install -o gateway -g gateway -m 0644 "${source_dir}/target_key-cert.pub" "${target_dir}/target_key-cert.pub"

for name in LABWEAVER_ACCESS_URL LABWEAVER_GATEWAY_IDENTITY LABWEAVER_ACCESS_CA_FILE LABWEAVER_SERVICE_OIDC_CA LABWEAVER_SERVICE_OIDC_ISSUER LABWEAVER_SERVICE_CLIENT_ID LABWEAVER_SERVICE_CLIENT_SECRET_FILE LABWEAVER_SERVICE_AUDIENCE LABWEAVER_SERVICE_SCOPES LABWEAVER_SERVICE_TOKEN_REFRESH_SKEW_SECONDS; do
    eval "value=\${${name}:-}"
    test -n "${value}"
    case "${value}" in
        *[!A-Za-z0-9_.,:@/-]*)
            echo "LW_GATEWAY_CONFIGURATION_INVALID: ${name}" >&2
            exit 1
            ;;
    esac
    printf 'export %s=%s\n' "${name}" "${value}"
done > "${target_dir}/gateway.env"
chown root:gateway-auth "${target_dir}/gateway.env"
chmod 0640 "${target_dir}/gateway.env"

exec /usr/sbin/sshd -D -e -f /etc/ssh/sshd_config
