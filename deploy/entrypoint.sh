#!/bin/sh
#
# Container entrypoint: make the mounted volume writable by the app user,
# then drop privileges and start the explorer. Cloud providers mount empty
# volumes with root ownership, so this must run as root before the app does.
set -eu

mkdir -p /data
chown -R app:app /data

# util-linux (setpriv) and coreutils (chown) are essential packages on
# Debian, so both are present in the slim runtime image.
exec setpriv --reuid=10001 --regid=10001 --clear-groups \
    /usr/local/bin/nvnmchain-explorer "$@"
