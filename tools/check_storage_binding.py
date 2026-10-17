"""Read-only CSI binding snapshot checks for #205; never author configuration."""
import argparse
import json
from pathlib import Path


class ObservationError(ValueError):
    """Input is missing, malformed, or internally inconsistent."""


def require(condition, code):
    if not condition:
        raise ObservationError(code)


def text(value):
    return isinstance(value, str) and bool(value.strip())


def object_parts(value, kind, api):
    require(isinstance(value, dict), "OBJECT_INVALID")
    require(value.get("kind") == kind and value.get("apiVersion") == api, "OBJECT_KIND_INVALID")
    metadata, spec = value.get("metadata"), value.get("spec")
    require(isinstance(metadata, dict), "METADATA_MISSING")
    require(all(text(metadata.get(key)) for key in ("name", "uid", "resourceVersion")),
            "OBJECT_IDENTITY_MISSING")
    require("deletionTimestamp" not in metadata, "OBJECT_DELETING")
    if kind != "StorageClass":
        require(isinstance(spec, dict), "SPEC_MISSING")
    return metadata, spec


def modes(value):
    require(isinstance(value, list) and bool(value), "ACCESS_MODES_MISSING")
    require(all(item in ("ReadWriteOnce", "ReadOnlyMany", "ReadWriteMany", "ReadWriteOncePod")
                for item in value), "ACCESS_MODES_INVALID")
    require(len(set(value)) == len(value), "ACCESS_MODES_INVALID")
    return set(value)


def inspect_binding(pvc, pv, storage_class):
    """Check three supplied API snapshots, not live state or fencing safety."""
    claim_meta, claim = object_parts(pvc, "PersistentVolumeClaim", "v1")
    volume_meta, volume = object_parts(pv, "PersistentVolume", "v1")
    class_meta, _ = object_parts(storage_class, "StorageClass", "storage.k8s.io/v1")
    require(text(claim_meta.get("namespace")), "CLAIM_NAMESPACE_MISSING")
    for obj in (pvc, pv):
        require(isinstance(obj.get("status"), dict) and obj["status"].get("phase") == "Bound",
                "BINDING_NOT_BOUND")
    require(claim.get("volumeName") == volume_meta["name"], "VOLUME_NAME_MISMATCH")
    reference = volume.get("claimRef")
    require(isinstance(reference, dict), "CLAIM_REFERENCE_MISSING")
    require(all(reference.get(key) == claim_meta[key] for key in ("name", "namespace", "uid")),
            "CLAIM_REFERENCE_MISMATCH")
    require(claim.get("storageClassName") == volume.get("storageClassName") == class_meta["name"],
            "STORAGE_CLASS_MISMATCH")
    csi = volume.get("csi")
    require(isinstance(csi, dict) and text(csi.get("driver")) and text(csi.get("volumeHandle")),
            "CSI_IDENTITY_MISSING")
    require(csi["driver"] == storage_class.get("provisioner"), "CSI_DRIVER_MISMATCH")
    require(modes(claim.get("accessModes")) <= modes(volume.get("accessModes")),
            "ACCESS_MODES_MISMATCH")
    claim_mode, volume_mode = claim.get("volumeMode", "Filesystem"), volume.get("volumeMode", "Filesystem")
    require(claim_mode in ("Filesystem", "Block") and claim_mode == volume_mode,
            "VOLUME_MODE_MISMATCH")
    return "BINDING_CONSISTENT_ONLY"


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "JSON_DUPLICATE_KEY")
        result[key] = value
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pvc", type=Path)
    parser.add_argument("pv", type=Path)
    parser.add_argument("storage_class", type=Path)
    args = parser.parse_args(argv)
    try:
        objects = [json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=unique_object)
                   for path in (args.pvc, args.pv, args.storage_class)]
        code = inspect_binding(*objects)
    except ObservationError as error:
        print(str(error))
        return 1
    except (OSError, UnicodeError, ValueError, RecursionError):
        print("OBSERVATION_INPUT_UNREADABLE")
        return 1
    print(code)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
