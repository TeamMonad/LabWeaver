# Sprint 2 deployment bundle

`sprint2-bundle-manifest.json` is the exact ConfigMap and Secret input contract for the destructive
Sprint 2 reset. Prepare values only under an ignored private directory using this layout:

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
command refuses to overwrite an existing bundle. The reset role validates the resulting object
and key set against the same manifest again before any cluster mutation.

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
