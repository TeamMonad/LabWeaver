#!/bin/sh
set -eu

package_dir=${0%/*}/..
package_dir=$(CDPATH= cd -- "$package_dir" && pwd)
work_dir=/work/labweaver-xv6
submission_dir=$work_dir/submission
evaluator_dir=$work_dir/evaluator
build_dir=/work/build

rm -rf "$work_dir" "$build_dir"
mkdir -p "$submission_dir/student" "$evaluator_dir/xv6" "$build_dir"
cp "$package_dir/student/student.c" "$submission_dir/student/student.c"
cp "$package_dir/xv6/xv6-source.tar.gz" "$evaluator_dir/xv6/xv6-source.tar.gz"

/usr/bin/sh "$package_dir/scripts/build-xv6.sh" \
    "$submission_dir/student/student.c" \
    "$build_dir/program" \
    "$submission_dir" \
    "$evaluator_dir"

initial_filesystem_hash=$(sha256sum "$build_dir/xv6/fs.img" | cut -d ' ' -f 1)

run_case() {
    case_name=$1
    case_dir=$work_dir/cases/$case_name
    mkdir -p "$case_dir"
    output=$(
        cd "$case_dir"
        /usr/bin/sh "$package_dir/scripts/run-xv6.sh" \
            "$build_dir/program" \
            "$submission_dir" \
            "$evaluator_dir" \
            < "$package_dir/tests/xv6-riscv64/$case_name.in"
    )
    expected=$(cat "$package_dir/tests/xv6-riscv64/$case_name.out")
    if [ "$output" != "$expected" ]; then
        echo "xv6 case $case_name failed" >&2
        echo "expected: $expected" >&2
        echo "actual: $output" >&2
        exit 1
    fi
    current_filesystem_hash=$(sha256sum "$build_dir/xv6/fs.img" | cut -d ' ' -f 1)
    if [ "$current_filesystem_hash" != "$initial_filesystem_hash" ]; then
        echo "xv6 case $case_name modified the shared filesystem image" >&2
        exit 1
    fi
    echo "xv6 case $case_name passed"
}

run_case smoke
run_case filesystem
