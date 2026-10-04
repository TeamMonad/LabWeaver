#!/bin/sh
set -eu

script_dir=${0%/*}
exec /usr/bin/python3 "$script_dir/local_test.py"
