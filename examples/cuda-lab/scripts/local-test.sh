#!/bin/sh
set -eu

package_dir=${0%/*}/..
package_dir=$(CDPATH= cd -- "$package_dir" && pwd)
work_dir=/work/labweaver-cuda
mkdir -p "$work_dir"

if [ ! -x /usr/local/cuda/bin/nvcc ]; then
    echo "cuda local test: nvcc is unavailable in this image" >&2
    exit 1
fi

/usr/local/cuda/bin/nvcc -O2 -o "$work_dir/gpu_stats" "$package_dir/student/gpu_stats.cu"
echo "cuda local test: nvcc compile passed"

output=$(/usr/bin/python3 "$package_dir/scripts/check_result.py" \
    "$package_dir/tests/cuda-nvcc/gpu-stats.out" \
    "$package_dir/tests/cuda-nvcc/expected_result.txt")
expected=$(cat "$package_dir/tests/cuda-nvcc/gpu-stats.out")
if [ "$output" != "$expected" ]; then
    echo "cuda local test: result checker mismatch" >&2
    echo "expected: $expected" >&2
    echo "actual: $output" >&2
    exit 1
fi
echo "cuda local test: result checker passed"
echo "cuda local test: run gpu_stats in a GPU environment and save stdout to student/result.txt"
