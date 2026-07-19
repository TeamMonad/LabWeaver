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
- `build-executor.yaml.example` → `build-executor-config/config.yaml`
- `environment-providers.json.example` → `environment-service-config/providers.json`
- `runtime-executor.yaml.example` → both runtime executor `config.yaml` files
- `web-deployment.json.example` → `web-config/deployment.json`

Environment-specific Helm values must explicitly bind adopted infrastructure names and VIPs. This
includes reviewed `hostAliases`, matching `/32` entries under
`network.externalServiceEndpoints`, and an opt-in `portalRoute`. The route creates a new
application HTTPRoute and ReferenceGrant; it does not replace a retained Gateway or existing
route. `sprint2-application` fails unless that route is both `Accepted` and `ResolvedRefs`.

The private Keycloak representation is imported when the realm is absent. For a retained realm,
the role uses `partialImport` with `ifResourceExists=SKIP`, then reads back the required client,
roles and users. Existing identities are not overwritten or deleted.
