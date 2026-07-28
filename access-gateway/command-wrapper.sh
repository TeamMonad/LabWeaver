#!/bin/sh
set -eu

. /etc/labweaver/secrets/gateway.env
exec /usr/local/bin/labweaver-gateway "$@"
