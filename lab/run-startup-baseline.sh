#!/bin/sh

# This script verifies the documented startup boundary without logging the key.
set -eu

readonly SERVER_IMAGE="${FERRUMTTY_LAB_SERVER_IMAGE:-ferrumtty-lab/mosh-server:1.4.0-arm64}"
readonly SERVER_PORT="${FERRUMTTY_LAB_SERVER_PORT:-60001}"
readonly SYNTHETIC_COMMAND='printf "FERRUMTTY-SYNTHETIC\n"; sleep 5'

docker run --rm "${SERVER_IMAGE}" new -p "${SERVER_PORT}" -- /bin/sh -lc "${SYNTHETIC_COMMAND}" \
    2>/dev/null \
    | while IFS= read -r startup_line; do
        case "${startup_line}" in
            'MOSH CONNECT '*) printf '%s\n' "${startup_line}" ;;
        esac
    done \
    | cargo run --quiet --package ferrumtty-lab -- verify-connect
