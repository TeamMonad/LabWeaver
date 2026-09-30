# CUDA GPU reduction lab

This package is a CUDA programming experiment. The student environment is a
CUDA devel image with an exclusive NVIDIA GPU attached through the platform GPU
allocation path. The starter `student/gpu_stats.cu` reduces the integers
`0..N-1` with `atomicAdd` and `atomicMax` but launches too few threads, so the
student must correct the coverage before submitting the program's statistics.

## GPU class selection

`environment.yaml` requests one GPU by catalog class. The class is a policy
identity; the active Resource catalog entry owns its provider binding, mode,
and runtime device mapping. Choose the class for the target environment when
publishing the package, and keep the requested class aligned with the catalog
entry and the environment mode. The package does not assume a particular GPU
model or worker.

The GPU resource field is additive to the existing CPU, memory, and storage
requirements. The student environment also declares an interactive terminal and
a read-only HTTP file entry over the workspace.

## Scoring contract

- The compile gate builds `student/gpu_stats.cu` with `/usr/local/cuda/bin/nvcc`.
  Compilation does not require a GPU.
- The score step does not execute the CUDA program. The Evaluation runner image
  contains the CUDA toolkit so that the profile can compile the source, but it
  has no GPU. The deterministic score reads the statistics from the submitted
  `student/result.txt` and compares `N`, `sum`, and `max` against the hidden
  expected values in `tests/cuda-nvcc/`.
- The expected statistics are `N=256`, `sum=32640`, `max=255`, worth 100 points.

## Package contents

- `environment.yaml` binds the non-root, read-only CUDA container with one
  exclusive GPU, a terminal, and one HTTP entry.
- `evaluation.yaml` collects the source and result file, gates on their
  presence, compiles with `nvcc`, and scores the submitted statistics.
- `profiles/cuda-nvcc.json` is the direct-argv profile with the absolute `nvcc`
  path and the Python result checker.
- `student/gpu_stats.cu` is the intentionally mis-launched starter.
- `tests/cuda-nvcc/` contains the evaluator input, expected output, and hidden
  expected statistics.
- `evaluation/Dockerfile` is the per-experiment Evaluation runner image with the
  CUDA toolkit and the platform worker binary.
- `manifest.json` records the SHA-256 of every payload file.

## Local verification

The repository local check compiles the starter with `nvcc` and validates the
result checker against the packaged fixtures. It does not exercise a GPU and
does not run the kernel.

```sh
docker build -t labweaver-cuda examples/cuda-lab
docker run --rm --read-only --network=none \
  -v "$PWD/examples/cuda-lab:/opt/labweaver:ro" \
  --tmpfs /work:rw,exec,uid=65534,gid=65534,mode=0777 \
  --tmpfs /tmp:uid=65534,gid=65534,mode=1777 \
  --tmpfs /workspace:uid=65534,gid=65534,mode=0777 \
  labweaver-cuda \
  /opt/labweaver/scripts/local-test.sh
```

A real GPU run requires the platform GPU allocation and an operator-supplied
catalog class. The base CUDA image tag `nvidia/cuda:12.4.1-devel-ubuntu22.04`
must be pinned to an image digest before publishing.
