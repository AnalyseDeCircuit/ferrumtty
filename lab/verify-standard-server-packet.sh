#!/bin/sh

# This script validates a standard server packet without retaining its key.
set -eu

readonly SERVER_IMAGE="${FERRUMTTY_LAB_SERVER_IMAGE:-ferrumtty-lab/mosh-server:1.4.0-arm64}"
readonly SERVER_PORT="${FERRUMTTY_LAB_SERVER_PORT:-60001}"
readonly SERVER_PACKET_INDEX="${FERRUMTTY_LAB_SERVER_PACKET_INDEX:-1}"
readonly SYNTHETIC_OUTPUT_BYTES="${FERRUMTTY_LAB_SYNTHETIC_OUTPUT_BYTES:-21}"
readonly SYNTHETIC_OUTPUT_MODE="${FERRUMTTY_LAB_SYNTHETIC_OUTPUT_MODE:-repeat}"
readonly PACKET_INSPECTION="${FERRUMTTY_LAB_PACKET_INSPECTION:-message}"

case ${PACKET_INSPECTION} in
    message) readonly VERIFY_COMMAND=verify-server-packet ;;
    fragment) readonly VERIFY_COMMAND=verify-server-fragment ;;
    *) printf 'invalid packet inspection mode\n' >&2; exit 2 ;;
esac

docker run --rm -i --cap-add NET_RAW \
    --env FERRUMTTY_LAB_SERVER_PORT="${SERVER_PORT}" \
    --env FERRUMTTY_LAB_SERVER_PACKET_INDEX="${SERVER_PACKET_INDEX}" \
    --env FERRUMTTY_LAB_SYNTHETIC_OUTPUT_BYTES="${SYNTHETIC_OUTPUT_BYTES}" \
    --env FERRUMTTY_LAB_SYNTHETIC_OUTPUT_MODE="${SYNTHETIC_OUTPUT_MODE}" \
    --entrypoint /bin/sh "${SERVER_IMAGE}" -seu <<'LAB_SCRIPT' \
    | cargo run --quiet --package ferrumtty-lab -- "${VERIFY_COMMAND}"
readonly server_port=${FERRUMTTY_LAB_SERVER_PORT}
readonly server_packet_index=${FERRUMTTY_LAB_SERVER_PACKET_INDEX}
readonly synthetic_output_bytes=${FERRUMTTY_LAB_SYNTHETIC_OUTPUT_BYTES}
readonly synthetic_output_mode=${FERRUMTTY_LAB_SYNTHETIC_OUTPUT_MODE}
readonly capture_file=/tmp/ferrumtty-packet.pcap
readonly server_capture_file=/tmp/ferrumtty-server-packet.pcap
readonly network_headers_bytes=48
readonly pcap_global_header_bytes=24
readonly pcap_record_header_bytes=16

tcpdump -i any -U -w "${capture_file}" udp port "${server_port}" >/dev/null 2>&1 &
capture_pid=$!
sleep 1
startup_output=$(/usr/bin/mosh-server new -p "${server_port}" -- /bin/sh -lc \
    'case $1 in
         repeat) dd if=/dev/zero bs=1 count="$2" 2>/dev/null | tr "\000" X ;;
         random) base64 /dev/urandom | dd bs=1 count="$2" 2>/dev/null ;;
         *) exit 64 ;;
     esac
     printf "\n"
     sleep 5' \
    ferrumtty-synthetic "${synthetic_output_mode}" "${synthetic_output_bytes}" 2>/dev/null)
connect_fields=$(printf '%s\n' "${startup_output}" \
    | awk '/^MOSH CONNECT / { print $3, $4; exit }')
unset startup_output
set -- ${connect_fields}
unset connect_fields
session_port=$1
session_key=$2

printf 'exit\n' \
    | MOSH_KEY="${session_key}" COLUMNS=80 LINES=24 timeout 8 script -q -e \
        -c "stty rows 24 cols 80; exec /usr/bin/mosh-client 127.0.0.1 ${session_port}" \
        /dev/null >/dev/null 2>&1
sleep 1
kill "${capture_pid}" 2>/dev/null || true
wait "${capture_pid}" 2>/dev/null || true

tcpdump -r "${capture_file}" -w "${server_capture_file}" \
    "src port ${server_port}" 2>/dev/null
case ${server_packet_index} in
    ''|*[!0-9]*|0) printf 'invalid server packet index\n' >&2; exit 2 ;;
esac
record_offset=${pcap_global_header_bytes}
current_index=1
while [ "${current_index}" -lt "${server_packet_index}" ]; do
    frame_bytes=$(od -An -tu4 -j $((record_offset + 8)) -N 4 \
        "${server_capture_file}" | tr -d ' ')
    record_offset=$((record_offset + pcap_record_header_bytes + frame_bytes))
    current_index=$((current_index + 1))
done
selected_frame_bytes=$(od -An -tu4 -j $((record_offset + 8)) -N 4 \
    "${server_capture_file}" | tr -d ' ')
payload_bytes=$((selected_frame_bytes - network_headers_bytes))
payload_offset=$((record_offset + pcap_record_header_bytes + network_headers_bytes))
payload_hex=$(dd if="${server_capture_file}" bs=1 skip="${payload_offset}" \
    count="${payload_bytes}" 2>/dev/null | od -An -v -tx1 | tr -d ' \n')

printf '%s\n%s\n' "${session_key}" "${payload_hex}"
unset session_key payload_hex
LAB_SCRIPT
