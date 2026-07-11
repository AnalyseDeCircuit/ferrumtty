#!/bin/sh

# This script confirms that the Linux laboratory supports traffic fault injection.
set -eu

readonly SERVER_IMAGE="${FERRUMTTY_LAB_SERVER_IMAGE:-ferrumtty-lab/mosh-server:1.4.0-arm64}"
readonly TEST_DELAY="${FERRUMTTY_LAB_DELAY:-100ms}"
readonly TEST_LOSS="${FERRUMTTY_LAB_LOSS:-1%}"
readonly TEST_DUPLICATION="${FERRUMTTY_LAB_DUPLICATION:-1%}"

docker run --rm --cap-add NET_ADMIN --entrypoint /bin/sh "${SERVER_IMAGE}" -eu -c '
    tc qdisc add dev eth0 root netem \
        delay "$1" \
        loss "$2" \
        duplicate "$3"
    tc qdisc show dev eth0
    tc qdisc del dev eth0 root
' ferrumtty-netem "${TEST_DELAY}" "${TEST_LOSS}" "${TEST_DUPLICATION}"
