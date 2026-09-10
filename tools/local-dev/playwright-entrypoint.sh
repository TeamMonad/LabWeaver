#!/bin/sh
set -eu

: "${LABWEAVER_CA_FILE:=/run/secrets/labweaver-ca/ca.crt}"
if [ ! -s "$LABWEAVER_CA_FILE" ]; then
    echo "LABWEAVER_CA_FILE is missing or empty" >&2
    exit 1
fi

# The container is disposable and starts as root.  Install the run-specific
# CA into the system bundle before Node or Chromium starts, then use an NSS
# database under a disposable HOME so Chromium trusts the same CA.
install -D -m 0444 "$LABWEAVER_CA_FILE" /usr/local/share/ca-certificates/labweaver-local-dev.crt
update-ca-certificates

export NODE_EXTRA_CA_CERTS="$LABWEAVER_CA_FILE"
export SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
export HOME=/tmp/labweaver-home
export XDG_CONFIG_HOME="$HOME/.config"

nss_database="$HOME/.pki/nssdb"
mkdir -p "$nss_database"
rm -f "$nss_database/cert9.db" "$nss_database/key4.db" "$nss_database/pkcs11.txt"
certutil -N -d "sql:$nss_database" --empty-password
certutil -A -d "sql:$nss_database" \
    -n labweaver-local-dev-ca \
    -t "C,," \
    -i "$LABWEAVER_CA_FILE"

if [ "$#" -eq 0 ]; then
    echo "a Playwright command is required" >&2
    exit 1
fi
exec "$@"
