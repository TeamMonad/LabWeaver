#!/bin/sh
set -eu

if [ "$#" -ne 4 ]; then
    echo "build-xv6: expected source, binary, submission directory, evaluator directory" >&2
    exit 64
fi

source_path=$1
binary_path=$2
submission_dir=$3
evaluator_dir=$4
build_dir=${binary_path%/*}
xv6_dir=$build_dir/xv6
archive_path=$evaluator_dir/xv6/xv6-source.tar.gz

case "$source_path" in
    "$submission_dir"/*) ;;
    *) echo "build-xv6: source is outside the submission directory" >&2; exit 65 ;;
esac
case "$binary_path" in
    /work/build/*) ;;
    *) echo "build-xv6: output is outside the build directory" >&2; exit 65 ;;
esac

if [ ! -f "$archive_path" ]; then
    echo "build-xv6: vendored source archive is unavailable" >&2
    exit 66
fi
if [ -e "$xv6_dir" ]; then
    echo "build-xv6: build directory is not empty" >&2
    exit 66
fi

mkdir "$xv6_dir"
tar -xzf "$archive_path" -C "$build_dir"
if [ ! -f "$xv6_dir/Makefile" ] || [ ! -f "$xv6_dir/kernel/kernel.ld" ]; then
    echo "build-xv6: source archive has an unexpected layout" >&2
    exit 66
fi
cp "$source_path" "$xv6_dir/user/student.c"

# Build the real xv6 kernel, filesystem image, and user program.  The source
# tree is extracted into the writable attempt build directory, so no package
# file is modified while make runs.
make -C "$xv6_dir" clean
make -C "$xv6_dir" kernel/kernel fs.img user/_student

printf 'xv6-riscv64 build complete\n' > "$binary_path"
