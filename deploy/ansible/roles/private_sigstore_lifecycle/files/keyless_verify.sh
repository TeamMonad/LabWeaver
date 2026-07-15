#!/bin/sh
set -eu
umask 077

required='KUBECONFIG KUBECTL COSIGN SIGSTORE_NAMESPACE IDENTITY_NAMESPACE IDENTITY_CA_SECRET IDENTITY_CLIENT_SECRET IDENTITY_CLIENT_ID OIDC_ISSUER OIDC_VIP SIGSTORE_HOST SIGSTORE_VIP EXPECTED_SUBJECT'
for name in $required; do
  eval "value=\${$name:-}"
  test -n "$value" || { echo "SIGSTORE_KEYLESS_INPUT_MISSING:$name" >&2; exit 2; }
done

work=$(mktemp -d)
exec 9>/run/labweaver-private-sigstore-hosts.lock
flock -x 9
cp /etc/hosts "$work/hosts"
cleanup() {
  cp "$work/hosts" /etc/hosts
  rm -rf "$work"
}
trap cleanup EXIT HUP INT TERM
printf '%s %s\n%s %s\n' "$OIDC_VIP" "${OIDC_ISSUER#https://}" "$SIGSTORE_VIP" "$SIGSTORE_HOST" \
  | sed 's#/.*##' >> /etc/hosts

secret_data() {
  namespace=$1
  secret=$2
  key=$3
  "$KUBECTL" --kubeconfig "$KUBECONFIG" -n "$namespace" get secret "$secret" \
    -o "jsonpath={.data.$key}" | base64 -d
}

secret_data "$IDENTITY_NAMESPACE" "$IDENTITY_CA_SECRET" 'tls\.crt' > "$work/keycloak-ca.pem"
secret_data "$IDENTITY_NAMESPACE" "$IDENTITY_CLIENT_SECRET" 'client-secret' > "$work/client-secret"
secret_data "$SIGSTORE_NAMESPACE" private-sigstore-internal-tls 'tls\.crt' > "$work/tls-chain.pem"
secret_data "$SIGSTORE_NAMESPACE" private-sigstore-fulcio-ca cert > "$work/fulcio-ca.pem"
secret_data "$SIGSTORE_NAMESPACE" private-sigstore-rekor-signer public > "$work/rekor.pem"
secret_data "$SIGSTORE_NAMESPACE" private-sigstore-ctlog-signer public > "$work/ctlog.pem"
for material in keycloak-ca.pem client-secret tls-chain.pem fulcio-ca.pem rekor.pem ctlog.pem; do
  test -s "$work/$material" || { echo "SIGSTORE_PUBLIC_MATERIAL_MISSING:$material" >&2; exit 3; }
done

"$COSIGN" trusted-root create \
  --no-default-fulcio --no-default-rekor --no-default-ctfe --no-default-tsa \
  --fulcio="url=https://$SIGSTORE_HOST,certificate-chain=$work/fulcio-ca.pem,start-time=1970-01-01T00:00:00Z" \
  --rekor="url=https://$SIGSTORE_HOST,public-key=$work/rekor.pem,start-time=1970-01-01T00:00:00Z" \
  --ctfe="url=https://$SIGSTORE_HOST,public-key=$work/ctlog.pem,start-time=1970-01-01T00:00:00Z,origin=sigstorescaffolding" \
  --out "$work/trusted-root.json" >/dev/null

oidc_host=${OIDC_ISSUER#https://}
oidc_host=${oidc_host%%/*}
{
  printf 'url = "%s/protocol/openid-connect/token"\n' "$OIDC_ISSUER"
  printf 'cacert = "%s"\n' "$work/keycloak-ca.pem"
  printf 'resolve = "%s:443:%s"\n' "$oidc_host" "$OIDC_VIP"
  printf 'request = "POST"\n'
  printf 'data = "grant_type=client_credentials"\n'
  printf 'data = "client_id=%s"\n' "$IDENTITY_CLIENT_ID"
  printf 'data = "client_secret=%s"\n' "$(cat "$work/client-secret")"
  printf 'fail\nsilent\nshow-error\n'
} > "$work/curl.conf"
curl --config "$work/curl.conf" > "$work/token.json"
python3 - "$work/token.json" "$work/token" <<'PY'
import json, pathlib, sys
source = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
token = source.get("access_token", "")
if token.count(".") != 2:
    raise SystemExit("SIGSTORE_OIDC_TOKEN_INVALID")
pathlib.Path(sys.argv[2]).write_text(token, encoding="utf-8")
PY

printf 'LabWeaver Issue 61 identity-bound TestFlight\n' > "$work/blob"
SSL_CERT_FILE="$work/tls-chain.pem" timeout --foreground --kill-after=5s 90s \
  "$COSIGN" sign-blob --yes --identity-token "$work/token" --use-signing-config=false \
  --trusted-root "$work/trusted-root.json" --fulcio-url "https://$SIGSTORE_HOST" \
  --rekor-url "https://$SIGSTORE_HOST" --bundle "$work/bundle.json" "$work/blob" \
  > "$work/signature"
SSL_CERT_FILE="$work/tls-chain.pem" timeout --foreground --kill-after=5s 90s \
  "$COSIGN" verify-blob --bundle "$work/bundle.json" --trusted-root "$work/trusted-root.json" \
  --certificate-identity "$EXPECTED_SUBJECT" --certificate-oidc-issuer "$OIDC_ISSUER" \
  "$work/blob" >/dev/null

python3 - "$work/bundle.json" <<'PY'
import json, pathlib, sys
bundle = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
if bundle.get("mediaType") != "application/vnd.dev.sigstore.bundle.v0.3+json":
    raise SystemExit("SIGSTORE_BUNDLE_MEDIA_TYPE_INVALID")
material = bundle.get("verificationMaterial", {})
if not material.get("certificate"):
    raise SystemExit("SIGSTORE_FULCIO_CERTIFICATE_MISSING")
entries = material.get("tlogEntries", [])
if not entries or not (entries[0].get("inclusionPromise") or entries[0].get("inclusionProof")):
    raise SystemExit("SIGSTORE_REKOR_INCLUSION_MISSING")
PY
printf '%s\n' '{"schema_version":"private-sigstore-keyless-check.v1","status":"passed","checks":["oidc","fulcio","ct-sct","rekor-inclusion","bundle-verification"]}'
