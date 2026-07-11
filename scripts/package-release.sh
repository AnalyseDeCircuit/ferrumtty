#!/bin/sh

# Build one target archive from the locked dependency graph.
set -eu
export LC_ALL=C

if [ "$#" -ne 2 ]; then
    printf '%s\n' 'usage: scripts/package-release.sh VERSION TARGET' >&2
    exit 2
fi

readonly version=$1
readonly target=$2
readonly archive_name="ferrumtty-${version}-${target}"
readonly staging_root="dist/${archive_name}"

case ${target} in
    *-windows-*) readonly executable_name=ferrumtty.exe ;;
    *) readonly executable_name=ferrumtty ;;
esac

cargo build --locked --release --package ferrumtty-client --target "${target}"
rm -rf "${staging_root}"
mkdir -p "${staging_root}"
cp "target/${target}/release/${executable_name}" "${staging_root}/"
case ${target} in
    *-windows-*) cp "${staging_root}/${executable_name}" "${staging_root}/mosh-client.exe" ;;
    *) cp "${staging_root}/${executable_name}" "${staging_root}/mosh-client" ;;
esac
cp LICENSE COPYRIGHT README.md README.zh-CN.md THIRD-PARTY-NOTICES.md "${staging_root}/"

tar -C dist -czf "dist/${archive_name}.tar.gz" "${archive_name}"
shasum -a 256 "dist/${archive_name}.tar.gz" >"dist/${archive_name}.tar.gz.sha256"
printf '%s\n' "dist/${archive_name}.tar.gz"
