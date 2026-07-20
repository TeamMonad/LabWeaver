# Sprint 2 deployment bundle

`sprint2-bundle-manifest.json` is the exact ConfigMap and Secret input contract for the
non-destructive Sprint 2 application adoption. Prepare values only under an ignored private
directory using this layout:

```text
.private/sprint2-input/
├── configmaps/<object-name>/<required-key>
└── secrets/<object-name>/<required-key>
```

Every object and key declared by the manifest is required. Extra objects, extra keys, symlinks,
empty files and files larger than 1 MiB are rejected. Generate the private bundle with:

```sh
python tools/render_sprint2_bundle.py \
  --input .private/sprint2-input \
  --output .private/sprint2-configuration-bundle.yaml
```

The command creates the output exclusively with mode `0600`, prints only its SHA-256 and object
count, and never logs Secret values. Both paths must be inside a `.private` directory, and the
command refuses to overwrite an existing bundle. The application role validates the resulting
object and key set before server-side applying only those application-owned objects. It does not
delete a namespace, database schema, NATS stream, MinIO bucket, Harbor project, Keycloak realm, or
retained infrastructure component.

The checked-in `*.example` files document non-secret runtime configuration fields. They are not a
deployable bundle and must not contain credentials.

Copy and specialize the examples as follows:

- `control-plane.yaml.example` → `control-service-config/config.yaml`
- `access-auth.yaml.example` → `access-service-config/config.yaml`
- `agent-control-plane.yaml.example` → `agent-service-config/config.yaml`
- `anthropic-base-url` → `agent-service-config/anthropic-base-url` (copy exactly)
- `build-executor.yaml.example` → `build-executor-config/config.yaml`
- `environment-providers.json.example` → `environment-service-config/providers.json`
- `runtime-executor.yaml.example` → both runtime executor `config.yaml` files
- `web-deployment.json.example` → `web-config/deployment.json`

Sprint 2 binds the pinned Claude Code CLI to ECNU's Anthropic-compatible endpoint. Put the
operator-provided `ECNU_API_KEY` value in
`secrets/agent-service-secrets/anthropic-auth-token` with mode `0600`; never put it in YAML, a
command argument, a log or a report. The Agent process receives only
`ANTHROPIC_BASE_URL` and `ANTHROPIC_AUTH_TOKEN` from the two mounted files. There is no ambient
credential fallback or alternate provider route.

Environment-specific Helm values must explicitly bind adopted infrastructure names and VIPs. This
includes reviewed `hostAliases`, matching `/32` entries under
`network.externalServiceEndpoints`, an opt-in `portalRoute`, and an opt-in
`sshGatewayRoute`. The routes create new application HTTPRoute/TCPRoute and ReferenceGrant
objects; they do not replace a retained Gateway or existing route. The application role may add
only the exact reviewed HTTPS and TCP/2222 listeners when they are absent and rejects a conflicting
listener. `sprint2-application` fails unless both enabled routes are `Accepted` and `ResolvedRefs`.
The portal authority is installed only in the retained router system trust so controller-side
verification never disables TLS validation.
When browser uploads are enabled, `objectStoreRoute` adds the bucket path to the same HTTPS
origin, preserves the signed Host and path, and uses `BackendTLSPolicy` to validate MinIO with the
reviewed internal CA. In that profile, only Control uses the portal origin as its object-store
endpoint; workers continue to use the cluster-internal MinIO endpoint. The route, CA ConfigMap,
ReferenceGrant and MinIO ingress allowance are additive application objects and do not replace or
reconfigure the retained MinIO service.
The HTTPS listener accepts routes only from namespaces labeled
`labweaver.io/gateway-routes=allowed`; the role adds that label to the reviewed portal namespace,
and the Container provider adds it to Environment-owned namespaces as part of the immutable plan.

The Container executor receives one mounted Harbor pull configuration and server-side applies it
as the fixed `harbor-course-pull` Secret inside each Environment-owned namespace before creating
the runtime ServiceAccount and Deployment. The credential is not included in an Environment plan,
NATS message, database row, report, or log. Missing, oversized, empty, or malformed Docker config
blocks the apply operation.

The private Keycloak representation is imported when the realm is absent. For a retained realm,
the role uses `partialImport` with `ifResourceExists=SKIP`, then reads back the required client,
roles and users. Existing identities are not overwritten or deleted.

The controller resolves retained headless PostgreSQL, NATS and MinIO endpoints from EndpointSlice
objects on every run and owns one bounded `/etc/hosts` block for those service DNS names plus the
reviewed Harbor and Keycloak VIPs. Administrative clients use explicit CA files and isolated
configuration directories; they do not disable TLS verification or depend on ambient credentials.
