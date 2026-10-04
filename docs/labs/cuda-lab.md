# CUDA Lab Material Contract

## Status and boundary

This document describes the teaching material in
[`examples/cuda-lab`](../../examples/cuda-lab). The package is a container-only
experiment with an exclusive GPU request. It runs through the ordinary
`container` runtime, the direct `program` runner, and the per-experiment
Evaluation runner image.

The student environment is built from the pinned CUDA devel image tag
`nvidia/cuda:12.4.1-devel-ubuntu22.04`. The tag must be resolved to an image
digest before publishing. The Evaluation runner image uses the same CUDA devel
base so that `profiles/cuda-nvcc.json` can invoke `/usr/local/cuda/bin/nvcc`.

## GPU catalog placeholder

`environment.yaml` requests one GPU of class `v100-exclusive`. That class is a
placeholder; the operator must either register the exclusive Tesla V100 worker
(worker-97, runtime device `nvidia.com/gpu`) under the catalog class
`v100-exclusive`, or change `resources.gpu.class` to the catalog class that is
actually active. The worker-158 P40 vGPU capacity is a different catalog class
and must not satisfy this package. The catalog entry, not the package, owns the
mapping from the policy class to the runtime device.

The GPU field is additive to CPU, memory, and storage. The environment also
declares an interactive terminal and a read-only HTTP file entry over the
workspace.

## Scoring contract

- The compile gate builds `student/gpu_stats.cu` with `nvcc`. Compilation does
  not require a GPU.
- The score step never executes the CUDA program. The Evaluation runner has no
  GPU, and the direct-argv `runArgv` reads the statistics from the submitted
  `student/result.txt` and compares `N`, `sum`, and `max` against the hidden
  expected values in `tests/cuda-nvcc/`.
- The expected statistics are `N=256`, `sum=32640`, `max=255`, worth 100
  points. The starter launches too few threads, so the student must correct the
  coverage and capture a real GPU run.
- A missing, malformed, or mismatched `student/result.txt` scores zero.

## Known runner constraint

The platform OJ compiler sandbox currently grants read access to `/usr/bin`,
`/usr/include`, `/usr/lib`, `/usr/libexec`, `/usr/lib64`,
`/usr/x86_64-pc-linux-gnu`, `/usr/share`, `/lib`, and `/lib64`. The CUDA
toolkit installs under `/usr/local/cuda`, which is outside that allowlist, so
`nvcc` cannot read its own installation under the current sandbox. The runner
implementation must add `/usr/local/cuda` (or `/usr/local`) to the compiler
read paths before this package can compile on the platform. The local check
compiles outside that sandbox and therefore does not cover this constraint.

## Verification

Run the package validator and the container self-check from the repository
root:

```sh
python tools/validate_approved_package.py examples/cuda-lab
```

The self-check needs the CUDA devel image and an `nvcc` binary. It compiles the
starter and validates the result checker against the packaged fixtures; it does
not exercise a GPU and does not run the kernel. A real GPU run requires the
platform GPU allocation and the operator-supplied catalog class.
