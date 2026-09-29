# vGPU guest image maintenance

The NVIDIA R580 guest package and the `gridd-unlock-patcher` binary are guest
image inputs. They are never installed on a Kubernetes node by the GPU device
plugin role. The image preparation command runs the existing locked Ubuntu
24.04 base through a disposable Docker builder, mounts only a caller-owned
working directory and `/dev/kvm`, and leaves the host driver and package state
unchanged.

Place the reviewed base disk, guest driver `.deb`, and patcher binary in one
working directory. The command verifies their SHA-256 values before starting
Docker; the values below are the inputs reviewed for the current V100 guest:

```sh
VGPU_WORKDIR="${VGPU_WORKDIR:?set a private working directory}"

python3 tools/prepare_vgpu_guest_image.py \
  --work-dir "$VGPU_WORKDIR" \
  --base-image "$VGPU_WORKDIR/ubuntu-vgpu.qcow2" \
  --driver-package "$VGPU_WORKDIR/nvidia-linux-grid-580_580.159.03_amd64.deb" \
  --patcher "$VGPU_WORKDIR/gridd-unlock-patcher-1.1" \
  --output "$VGPU_WORKDIR/ubuntu-vgpu-580.159.03.containerdisk.tar" \
  --prepared-disk "$VGPU_WORKDIR/ubuntu-vgpu-580.159.03.qcow2"
```

The builder checks the source virtual size with `qemu-img info`, adds four GiB
of package headroom, and uses `virt-resize` to expand the inspected Ubuntu
root partition (or its single LVM PV and root LV). Unsupported or ambiguous
layouts fail before package installation. It installs `dkms`, matching Ubuntu
kernel headers and image, the reviewed driver package, and the patcher under
`/usr/local/bin/gridd-unlock-patcher`. It removes temporary package files
before creating the output. `virt-sysprep` clears machine IDs, network hardware
addresses, and SSH host keys so a VM does not inherit an identity from the
source disk.

The disposable builder includes the `iproute2`, `dhcpcd-base`, and
`isc-dhcp-client` packages required by the libguestfs appliance's normal DHCP
startup path. `virt-customize --network` therefore obtains its resolver and
default route from the appliance network; the builder does not embed a campus
DNS address. Guest package installation uses APT's `Error-Mode=any` so a
partial repository update fails the build instead of producing a partially
prepared image.

The tar archive contains exactly one regular file at `disk/disk.qcow2`, which
is the `diskPath` to provide to the existing Agent platform-image import. The
generated JSON beside the archive records the input hashes, prepared disk
hash, `qcow2` format, and the actual virtual capacity reported by the builder.
Import it as a VM image with the reviewed binding and registry reference;
publishing remains an explicit administrator operation after the local output
is inspected.

The command deliberately has no DLS token, TLS CA, or DLS signing root input.
The VM bootstrap retrieves the client token and DLS signing root from the
Secret references exposed by the `fastapi-dls-client` ConfigMap before applying
the patcher. The signing root used by `gridd-unlock-patcher` is different from
the nginx TLS `ca.crt` in `fastapi-dls-tls`; never substitute the TLS CA. The
token and certificate are runtime material and must not be copied into the
guest image or repository.

The optional cluster-internal FastAPI-DLS role uses the stable references
`fastapi-dls-client-token` (Secret key `client-token`),
`fastapi-dls-tls` (key `ca.crt` for nginx TLS trust), and
`fastapi-dls-signing-root` (key `ca.crt` for the guest patcher). Its backend
management endpoints remain loopback-only; guest traffic is limited to
`/auth/v1/` and `/leasing/v1/`.
