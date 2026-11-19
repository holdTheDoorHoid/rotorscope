#!/usr/bin/env bash
# Check the Espressif radio libraries every radio firmware links, and the patched esp-radio
# copy, against what docs/LINK-KEYS-DESIGN.md section 8 records. Offline: it reads only the
# lockfiles, the local cargo registry and this repository.
#
#   tools/check-wifi-blobs.sh [lockfile...]   default: every firmware/*/Cargo.lock
#   tools/check-wifi-blobs.sh --self-test     checks of this script on fake data (no registry)
#
# 1. Radio library blobs (CVE-2025-52471). The Wi-Fi libraries come as binary archives inside
#    the `esp-wifi-sys-<chip>` crates. For every such crate a lockfile names, for the chips our
#    firmware is built for (ESP32-C3, ESP32-C5, ESP32-S3), the git blob hashes of
#    libespnow.a, libnet80211.a and libpp.a in the local registry must equal the list below.
#    Those hashes are the files of espressif/esp32-wifi-lib at 7cd4d7b, the library commit
#    ESP-IDF v6.1-255-g4dbfecac7e3 pins, which contains the CVE fix (ESP-IDF edc227c). So a
#    dependency bump cannot silently go back to a library without the fix: a new crate
#    version has no entry here and fails until someone checks its provenance and records it.
#    The other chips appear in the lockfiles only as optional dependencies nothing builds;
#    they are listed, not checked.
# 2. Patched esp-radio. Every lockfile that uses esp-radio must take the patched copy
#    (firmware/patches/esp-radio, no registry source), and that copy must differ from the
#    crates.io release only in the two files firmware/patches/README.md names. The release is
#    read from the registry's download cache (.crate) after checking its SHA-256 against the
#    checksum the lockfiles recorded before the patch.
#
# Exit 0: all good. Exit 1: something failed (each failure says what and why).
# Environment: CARGO_HOME (default ~/.cargo); CHECK_WIFI_BLOBS_LIST (a file of
# "chip version file hash" lines replacing the list below; self-test only).
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(dirname "$here")

# chip version file git-blob-hash (git hash-object). Checked against
# github.com/espressif/esp32-wifi-lib at 7cd4d7b on 2026-11-18: identical.
RECORDED='
esp32c3 0.3.0 libespnow.a   f9e4d48d42810e010aebb2330c9b149da7ad478c
esp32c3 0.3.0 libnet80211.a fedc7af8d98e6ed9f25e83ece00c4b5436fd205c
esp32c3 0.3.0 libpp.a       970e34ec35ac8d17509fe8a6093ec86067b8af4c
esp32c5 0.3.0 libespnow.a   b085d2d8265c8d78baf9a3d05ff312a9d047f7a3
esp32c5 0.3.0 libnet80211.a 12e0f60a32766dd48e57e78b5e8d0b4fdf87bed7
esp32c5 0.3.0 libpp.a       95e0cb8c4ca8bea21d0edd66d511ea5644215c11
esp32s3 0.3.0 libespnow.a   5804eddb93b3144781f1457e91722a7a19b77ad5
esp32s3 0.3.0 libnet80211.a a19d4e6cedf2a6c7da64762801e846c9347971dc
esp32s3 0.3.0 libpp.a       a1364a6bbdb234699f28546d0ed01ad755f9931c
'
CHIPS=(esp32c3 esp32c5 esp32s3)
FILES=(libespnow.a libnet80211.a libpp.a)

# The esp-radio release the patched copy is made from, and its crates.io checksum.
ESP_RADIO_VERSION=1.0.0-beta.1
ESP_RADIO_SHA256=50247f4e91000fc87661e77254126c74acc4535f822ee247f7778807202dc938
# What may differ between the release and the patched copy (diff -rq lines, normalised).
ESP_RADIO_DIFFS='only-release .cargo_vcs_info.json
only-release Cargo.lock
differ src/esp_now/mod.rs
differ src/radio_clocks/clocks_ll/esp32c5.rs'

cargo_home=${CARGO_HOME:-$HOME/.cargo}
fails=0
fail() { echo "FAIL $*" >&2; fails=$((fails + 1)); }

# git's blob hash, without needing git: sha1("blob <size>\0" + contents).
blob_hash() {
  local f=$1
  { printf 'blob %s\0' "$(wc -c <"$f" | tr -d ' ')"; cat "$f"; } | sha1sum | cut -d' ' -f1
}

recorded() {  # recorded <chip> <version> <file>: the hash, or nothing
  local list=$RECORDED
  [[ -n ${CHECK_WIFI_BLOBS_LIST:-} ]] && list=$(cat "$CHECK_WIFI_BLOBS_LIST")
  awk -v c="$1" -v v="$2" -v f="$3" '$1 == c && $2 == v && $3 == f { print $4 }' <<<"$list"
}

# "name version source" for every package in a lockfile (source "-" for path crates).
lock_packages() {
  awk '
    /^\[\[package\]\]/ { if (n != "") print n, v, (s == "" ? "-" : s); n = v = s = ""; next }
    /^name = /    { gsub(/"/, "", $3); n = $3 }
    /^version = / { gsub(/"/, "", $3); v = $3 }
    /^source = /  { gsub(/"/, "", $3); s = $3 }
    END { if (n != "") print n, v, (s == "" ? "-" : s) }
  ' "$1"
}

registry_dir() {  # registry_dir <crate> <version>: the unpacked crate, or nothing
  local d
  for d in "$cargo_home"/registry/src/*/"$1-$2"; do
    [[ -d $d ]] && { echo "$d"; return 0; }
  done
  return 0
}

check_blobs() {  # check_blobs <lockfile>
  local lock=$1 name ver src chip dir f want got skipped=()
  while read -r name ver src; do
    [[ $name == esp-wifi-sys-* ]] || continue
    chip=${name#esp-wifi-sys-}
    if [[ " ${CHIPS[*]} " != *" $chip "* ]]; then skipped+=("$chip"); continue; fi
    dir=$(registry_dir "$name" "$ver")
    if [[ -z $dir ]]; then
      fail "$lock: $name $ver is not in the local cargo registry ($cargo_home/registry/src): cannot check its radio libraries. Fetch it once (cargo fetch in that firmware directory, with network), then run this again."
      continue
    fi
    for f in "${FILES[@]}"; do
      want=$(recorded "$chip" "$ver" "$f")
      if [[ -z $want ]]; then
        fail "$lock: no recorded hash for $name $ver $f. A dependency bump? Check the new libraries' provenance against the CVE-2025-52471 fix (docs/LINK-KEYS-DESIGN.md section 8) before recording them here."
        continue
      fi
      if [[ ! -f $dir/libs/$f ]]; then
        fail "$lock: $dir/libs/$f is missing"
        continue
      fi
      got=$(blob_hash "$dir/libs/$f")
      if [[ $got != "$want" ]]; then
        fail "$lock: $name $ver $f has hash $got, recorded $want: NOT the library with the CVE-2025-52471 fix that was checked"
      else
        echo "ok   $(basename "$(dirname "$lock")"): $name $ver $f"
      fi
    done
  done < <(lock_packages "$lock")
  (( ${#skipped[@]} )) && echo "     $(basename "$(dirname "$lock")"): not built for, not checked: ${skipped[*]}"
  return 0
}

check_patch_entry() {  # check_patch_entry <lockfile>: esp-radio, if used, is the patched copy
  local lock=$1 name ver src
  while read -r name ver src; do
    [[ $name == esp-radio ]] || continue
    if [[ $src != - ]]; then
      fail "$lock: esp-radio $ver comes from $src, not firmware/patches/esp-radio: add the [patch.crates-io] entry (firmware/patches/README.md)"
    elif [[ $ver != "$ESP_RADIO_VERSION" ]]; then
      fail "$lock: patched esp-radio is $ver, expected $ESP_RADIO_VERSION"
    else
      echo "ok   $(basename "$(dirname "$lock")"): esp-radio is the patched copy"
    fi
  done < <(lock_packages "$lock")
}

check_patched_copy() {
  local crate t got
  crate=$(ls "$cargo_home"/registry/cache/*/"esp-radio-$ESP_RADIO_VERSION.crate" 2>/dev/null | head -1 || true)
  if [[ -z $crate ]]; then
    fail "esp-radio $ESP_RADIO_VERSION.crate is not in the local cargo registry cache ($cargo_home/registry/cache): cannot compare the patched copy with the release. Download it once from crates.io (its SHA-256 must be $ESP_RADIO_SHA256) into that cache, or fetch it with cargo."
    return
  fi
  got=$(sha256sum "$crate" | cut -d' ' -f1)
  if [[ $got != "$ESP_RADIO_SHA256" ]]; then
    fail "$crate has SHA-256 $got, not the crates.io release's $ESP_RADIO_SHA256"
    return
  fi
  t=$(mktemp -d)
  tar -xzf "$crate" -C "$t"
  local diffs
  diffs=$(cd "$t" && diff -rq "esp-radio-$ESP_RADIO_VERSION" "$root/firmware/patches/esp-radio" 2>&1 |
    sed -E "s#^Only in esp-radio-$ESP_RADIO_VERSION(/([^:]*))?: (.*)#only-release \2/\3#;
            s#^Only in $root/firmware/patches/esp-radio(/([^:]*))?: (.*)#only-patched \2/\3#;
            s#^Files esp-radio-$ESP_RADIO_VERSION/(.*) and .* differ#differ \1#;
            s#(only-[a-z]+ )/#\1#" | sort || true)
  rm -rf "$t"
  if [[ $diffs != "$(sort <<<"$ESP_RADIO_DIFFS")" ]]; then
    fail "firmware/patches/esp-radio differs from the esp-radio $ESP_RADIO_VERSION release in more (or fewer) places than firmware/patches/README.md names:"
    diff <(sort <<<"$ESP_RADIO_DIFFS") <(echo "$diffs") >&2 || true
    return
  fi
  if ! grep -q 'ROTORSCOPE PATCH: CVE-2025-52471' "$root/firmware/patches/esp-radio/src/esp_now/mod.rs"; then
    fail "firmware/patches/esp-radio/src/esp_now/mod.rs lacks the receive-length check"
    return
  fi
  echo "ok   firmware/patches/esp-radio = esp-radio $ESP_RADIO_VERSION (SHA-256 checked) + the two documented changes"
}

self_test() {
  local t out rc failed=0
  set +e  # the checks look at failures
  t=$(mktemp -d); trap 'rm -rf "$t"' RETURN
  mkdir -p "$t/home/registry/src/index.test/esp-wifi-sys-esp32c5-0.3.0/libs" "$t/fw"
  for f in "${FILES[@]}"; do printf 'blob %s\n' "$f" >"$t/home/registry/src/index.test/esp-wifi-sys-esp32c5-0.3.0/libs/$f"; done
  : >"$t/list"
  for f in "${FILES[@]}"; do
    echo "esp32c5 0.3.0 $f $(blob_hash "$t/home/registry/src/index.test/esp-wifi-sys-esp32c5-0.3.0/libs/$f")" >>"$t/list"
  done
  printf '[[package]]\nname = "esp-wifi-sys-esp32c5"\nversion = "0.3.0"\nsource = "registry+x"\n\n[[package]]\nname = "esp-wifi-sys-esp32h2"\nversion = "0.3.0"\nsource = "registry+x"\n\n[[package]]\nname = "esp-radio"\nversion = "1.0.0-beta.1"\n' >"$t/fw/Cargo.lock"
  sub() { CARGO_HOME=$t/home CHECK_WIFI_BLOBS_LIST=$t/list "$here/check-wifi-blobs.sh" --blobs-only "$@" 2>&1; }
  check() { if [[ $2 == 0 ]]; then echo "ok   $1"; else echo "FAIL $1"; failed=$((failed + 1)); fi; }
  # The hash function is git's.
  [[ $(printf 'hello\n' >"$t/h"; blob_hash "$t/h") == ce013625030ba8dba906f756967f9e9ca394464a ]]; check "blob hash is git's" $?
  out=$(sub "$t/fw/Cargo.lock"); rc=$?
  [[ $rc == 0 ]] && grep -q 'not checked: esp32h2' <<<"$out"; check "matching blobs pass; unbuilt chips listed" $?
  printf 'x' >>"$t/home/registry/src/index.test/esp-wifi-sys-esp32c5-0.3.0/libs/libpp.a"
  out=$(sub "$t/fw/Cargo.lock"); rc=$?
  [[ $rc == 1 ]] && grep -q 'libpp.a has hash .* NOT the library' <<<"$out"; check "a wrong hash fails" $?
  sed -i 's/0.3.0/0.4.0/' "$t/fw/Cargo.lock"
  out=$(sub "$t/fw/Cargo.lock"); rc=$?
  [[ $rc == 1 ]] && grep -q 'esp-wifi-sys-esp32c5 0.4.0 is not in the local cargo registry' <<<"$out"; check "a crate missing from the registry fails clearly" $?
  mkdir -p "$t/home/registry/src/index.test/esp-wifi-sys-esp32c5-0.4.0/libs"
  cp "$t/home/registry/src/index.test/esp-wifi-sys-esp32c5-0.3.0/libs/"* "$t/home/registry/src/index.test/esp-wifi-sys-esp32c5-0.4.0/libs/"
  out=$(sub "$t/fw/Cargo.lock"); rc=$?
  [[ $rc == 1 ]] && grep -q 'no recorded hash for esp-wifi-sys-esp32c5 0.4.0' <<<"$out"; check "an unrecorded version fails" $?
  printf '[[package]]\nname = "esp-radio"\nversion = "1.0.0-beta.1"\nsource = "registry+x"\n' >"$t/fw/Cargo.lock"
  out=$(sub "$t/fw/Cargo.lock"); rc=$?
  [[ $rc == 1 ]] && grep -q 'add the \[patch.crates-io\] entry' <<<"$out"; check "an unpatched esp-radio fails" $?
  (( failed == 0 )) && echo "check-wifi-blobs self-test: all passed" || echo "check-wifi-blobs self-test: $failed FAILED"
  return $(( failed > 0 ))
}

blobs_only=0
case ${1:-} in
  --self-test) self_test; exit $? ;;
  --blobs-only) blobs_only=1; shift ;;  # self-test helper: no patched-copy check
esac

locks=("$@")
if (( ${#locks[@]} == 0 )); then
  for l in "$root"/firmware/*/Cargo.lock; do locks+=("$l"); done
fi
for l in "${locks[@]}"; do
  [[ -f $l ]] || { fail "$l: no such lockfile"; continue; }
  check_blobs "$l"
  check_patch_entry "$l"
done
(( blobs_only )) || check_patched_copy
if (( fails )); then
  echo "check-wifi-blobs: $fails FAILED" >&2
  exit 1
fi
echo "check-wifi-blobs: all passed"
