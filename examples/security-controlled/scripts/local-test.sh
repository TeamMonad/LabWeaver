#!/bin/sh
set -eu

package_dir=${0%/*}/..
package_dir=$(CDPATH= cd -- "$package_dir" && pwd)
work_dir=/work/security-controlled
student_binary=$work_dir/bin/student-auth
vulnerable_binary=$work_dir/bin/vulnerable-auth
fixed_binary=$work_dir/bin/fixed-auth

rm -rf "$work_dir"
mkdir -p "$work_dir/bin" "$work_dir/cases/valid-logins" \
    "$work_dir/cases/authentication-bypass" "$work_dir/cases/parser-boundary"

compile() {
    source_path=$1
    binary_path=$2
    /usr/bin/gcc -std=c17 -O2 -Wall -Wextra -Werror \
        "$source_path" -o "$binary_path"
}

compile "$package_dir/student/auth.c" "$student_binary"
compile "$package_dir/reference/vulnerable_auth.c" "$vulnerable_binary"
compile "$package_dir/reference/fixed_auth.c" "$fixed_binary"

run_inline() {
    case_name=$1
    binary_path=$2
    input_line=$3
    expected=$4
    output=$(printf '%s\n' "$input_line" | timeout --kill-after=1s 2s "$binary_path")
    if [ "$output" != "$expected" ]; then
        echo "security demonstration $case_name failed" >&2
        echo "expected: $expected" >&2
        echo "actual: $output" >&2
        exit 1
    fi
    echo "security demonstration $case_name passed"
}

run_case() {
    case_name=$1
    binary_path=$2
    output=$(
        cd "$work_dir/cases/$case_name"
        timeout --kill-after=1s 2s "$binary_path" \
            < "$package_dir/tests/security-controlled/$case_name.in"
    )
    expected=$(cat "$package_dir/tests/security-controlled/$case_name.out")
    if [ "$output" != "$expected" ]; then
        echo "security case $case_name failed" >&2
        echo "expected: $expected" >&2
        echo "actual: $output" >&2
        exit 1
    fi
    echo "security case $case_name passed"
}

run_inline vulnerable-template "$vulnerable_binary" \
    'operator:training-password' \
    'access-granted TRAINING_FLAG_OFFLINE_AUTH_DEMO'
run_inline fixed-reference "$fixed_binary" \
    'operator:training-password' \
    'access-denied'

run_inline student-template "$student_binary" \
    'operator:training-password' \
    'access-granted TRAINING_FLAG_OFFLINE_AUTH_DEMO'

run_case valid-logins "$fixed_binary"
run_case authentication-bypass "$fixed_binary"
run_case parser-boundary "$fixed_binary"
