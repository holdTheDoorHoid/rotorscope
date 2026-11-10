#!/usr/bin/env bash
# Fetch the two public DJI DroneID recordings from RUB-SysSec/DroneSecurity
# (NDSS 2023) into testdata/droneid/ for crates/droneid's integration tests,
# pinned to one commit and checked by SHA-256.
#
# The recordings are AGPL-3.0 repository content: they are downloaded, never
# committed (testdata/ is gitignored). Both are 50 MS/s complex64 (float32
# I/Q, little-endian).
#
#   tools/fetch-droneid-samples.sh            # download from GitHub
#   tools/fetch-droneid-samples.sh --from DIR # copy from a local clone's samples/
set -euo pipefail

COMMIT=9ff819843bee48fb140a0704ec78aff757896dea
BASE="https://raw.githubusercontent.com/RUB-SysSec/DroneSecurity/${COMMIT}/samples"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="${ROOT}/testdata/droneid"

FILES=(
  "mavic_air_2 623fa40f190d3fd859ec2fd62b379ad9a01a0485db3179ae29ac2e3c7a070450"
  "mini2_sm 5a17913cb11cde2919347054700345c9e5595eaff7818980d332b8ac8910bc55"
)

FROM=""
if [[ "${1:-}" == "--from" ]]; then
  FROM="${2:?--from needs a directory}"
fi

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

mkdir -p "$DEST"
for entry in "${FILES[@]}"; do
  read -r name want <<<"$entry"
  out="${DEST}/${name}"
  if [[ -f "$out" && "$(sha256 "$out")" == "$want" ]]; then
    echo "ok       ${name} (already present)"
    continue
  fi
  tmp="${out}.part"
  if [[ -n "$FROM" ]]; then
    cp "${FROM}/${name}" "$tmp"
  else
    curl --fail --location --silent --show-error -o "$tmp" "${BASE}/${name}"
  fi
  got="$(sha256 "$tmp")"
  if [[ "$got" != "$want" ]]; then
    rm -f "$tmp"
    echo "FAILED   ${name}: sha256 ${got}, expected ${want}" >&2
    exit 1
  fi
  mv "$tmp" "$out"
  echo "fetched  ${name} ($(wc -c <"$out") bytes, sha256 verified)"
done
echo "recordings in ${DEST}"
