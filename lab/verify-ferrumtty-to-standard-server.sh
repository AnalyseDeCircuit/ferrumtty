#!/bin/sh

# This script keeps the key in a pipe between the standard server and FerrumTTY.
set -eu

readonly SERVER_IMAGE="${FERRUMTTY_LAB_SERVER_IMAGE:-ferrumtty-lab/mosh-server:1.4.0-arm64}"
readonly SERVER_PORT="${FERRUMTTY_LAB_SERVER_PORT:-60001}"
readonly SERVER_LOSS="${FERRUMTTY_LAB_SERVER_LOSS:-0%}"

docker run --rm -i --cap-add NET_ADMIN \
    --publish "127.0.0.1:${SERVER_PORT}:${SERVER_PORT}/udp" \
    --env FERRUMTTY_LAB_SERVER_PORT="${SERVER_PORT}" \
    --env FERRUMTTY_LAB_SERVER_LOSS="${SERVER_LOSS}" \
    --entrypoint /bin/sh "${SERVER_IMAGE}" -seu <<'LAB_SCRIPT' \
    | cargo run --quiet --package ferrumtty-lab -- connect-standard-server
readonly server_port=${FERRUMTTY_LAB_SERVER_PORT}
readonly server_loss=${FERRUMTTY_LAB_SERVER_LOSS}
tc qdisc add dev eth0 root netem loss "${server_loss}"
startup_output=$(/usr/bin/mosh-server new -i 0.0.0.0 -p "${server_port}" -- /bin/sh -lc \
    'printf "FERRUMTTY-SYNTHETIC\n"; sleep 10' 2>/dev/null)
printf '%s\n' "${startup_output}" | while IFS= read -r startup_line; do
    case ${startup_line} in
        'MOSH CONNECT '*) printf '%s\n' "${startup_line}" ;;
    esac
done
unset startup_output
while pgrep -x mosh-server >/dev/null 2>&1; do
    sleep 1
done
LAB_SCRIPT
