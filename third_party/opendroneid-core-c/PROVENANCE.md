# Provenance of this copy of opendroneid-core-c

This folder is a copy of [opendroneid/opendroneid-core-c](https://github.com/opendroneid/opendroneid-core-c),
the Open Drone ID reference library, under its own licence (Apache-2.0, `LICENSE` here). Only
`crates/odid-ref` uses it, in host-side tests; it is never built into firmware.

This file is Rotorscope's, not upstream's.

## What is known (as of 2026-11-19)

| Fact | Value | How it is known |
|---|---|---|
| Upstream repository | `https://github.com/opendroneid/opendroneid-core-c` | Its own `README.md` and `.gitmodules` |
| Added to Rotorscope | commit `44b141e` (2026-11-09), "Project skeleton" | `git log -- third_party/opendroneid-core-c`; no later commit has changed a file here |
| Version string | `VERSION 0.2` in `CMakeLists.txt` | Upstream has kept this number for years, so it does not identify a commit |
| Protocol version | `ODID_PROTOCOL_VERSION 2` (ASTM F3411-22a) in `libopendroneid/opendroneid.h` | |
| Newest content | the GB 46750-2025 (China) library `libopendroneidcn/`, copyright 2026, and a README paragraph saying that standard took effect on 2026-05-01 | So the copy was taken from upstream after that library landed, in 2026 |
| Git tree of this folder | `abd2eade535afde9263635d2b99c9fd8db9bcef8` (before this file was added) | `git rev-parse 44b141e:third_party/opendroneid-core-c` |
| Submodule | `mavlink_c_library_v2` is listed in `.gitmodules` but not copied | Rotorscope does not build `libmav2odid` |

## What is not known

- **The upstream commit.** Nothing in the files names it, and the copy has no `.git` folder.
- **Whether the copy is complete and unchanged.** Rotorscope has not changed it since it was added,
  but whether files were left out or edited when it was copied in cannot be told without upstream.

## Git blob hashes of the copied files

Each hash is `git hash-object <file>`, the same id upstream's Git history holds for an identical
file.

| Blob | File |
|---|---|
| `97f6fdbc4d1c7a5792d6cbea2be258427ab7da82` | `.editorconfig` |
| `40fb72d9e306b37330c1a4bf4eceb14eb39f8fd7` | `.github/workflows/cmake.yml` |
| `b9dc394a5553bea08af9b15a3ee1839eeeeb6869` | `.gitignore` |
| `21512c725d021b0da6bd32c16fa4377a6926b641` | `.gitmodules` |
| `44726bd42b3bc34dcb2de8d3b20074675617defd` | `CMakeLists.txt` |
| `261eeb9e9f8b2b4b0d119366dda99c6fd7d35c64` | `LICENSE` |
| `73099038a341271c326ac5b34654ac15cb8eede6` | `README.md` |
| `7d95b1fda3ed1ea5e9fd3d527ec55546d7d34436` | `img/core-arch.png` |
| `f58509979811c383ddd97a9968234d5a625e8333` | `libmav2odid/CMakeLists.txt` |
| `c1130fcb4cb7a37ebbbcf96ae25a8c5db0aa0fb4` | `libmav2odid/libmav2odid.pc.cmake` |
| `6710981fc778cab0ecbe21aa53e93d647b2b0686` | `libmav2odid/mav2odid.c` |
| `c5c6541b60b4c40f2d5dc2e29b57d00890604523` | `libmav2odid/mav2odid.h` |
| `5f376897bb32691bb101d062e60413181c3c5fe5` | `libopendroneid/CMakeLists.txt` |
| `3a126aec8b4366f30b143bb4a7e43ded415b27ee` | `libopendroneid/libopendroneid.pc.cmake` |
| `c51765802c99407e26883515d3836b460bb7241b` | `libopendroneid/odid_wifi.h` |
| `13f7e2bb9cc036f09b88d3c31bd0f489f3862692` | `libopendroneid/opendroneid.c` |
| `0c11f57923e4861ca7b171c87aaf29bfed9183a3` | `libopendroneid/opendroneid.h` |
| `6d40fd4c31706e944738ba831bd99b74334aba1c` | `libopendroneid/wifi.c` |
| `7d59717eb275c2ece5ae6dc9333836aa21d613f4` | `libopendroneidcn/CMakeLists.txt` |
| `e3a57559c8ee51f144a2c48e3feb06e02690f47f` | `libopendroneidcn/README.md` |
| `c3d228641c4c47c3d2f677a65daa1cb47bf64f15` | `libopendroneidcn/libopendroneidcn.pc.cmake` |
| `5cf35622f7ffe0ac7480e903fdc648c9317777a2` | `libopendroneidcn/opendroneidcn.c` |
| `aa6205fb2cef254f660b6c3ecc9746ce472f73e2` | `libopendroneidcn/opendroneidcn.h` |
| `c5b9468c5d8cba9eee11b51001f8a65bde61310e` | `libopendroneidcn/opendroneidcn_demo.c` |
| `161329a5530e325bb8815385ef8ddb6ef00b1ebe` | `test/CMakeLists.txt` |
| `662c71a8724d7bd77d4aa350dde0023d14670843` | `test/conan.cmake` |
| `dd1dc110cf582167ff0256dc8d75b1275113e6e0` | `test/main.c` |
| `aff9b948701adabce0efdc502e577bd39bab6c4e` | `test/opendroneid_sim.c` |
| `2ccc6a7d36e293023a37fadc3e92e72df8dba553` | `test/test_inout.c` |
| `8fa55e09c122c7a70804486cf5c3c263f1b9cd30` | `test/test_mav2odid.c` |
| `1d0b48c091bf284fc8745e063f00e52d633bd234` | `test/unit_odid_wifi_beacon.cpp` |
| `c1287727cff82dc8cfe1589716ea405db6dbcb47` | `wifi/CMakeLists.txt` |
| `136bbd68743d31c722d87017df6408298b9fb0b4` | `wifi/README.md` |
| `b2e74bc94b71099aa76232188649b1e24926e52c` | `wifi/sender/CMakeLists.txt` |
| `b094d76f03d255cdc67fee1781f53a0294158c19` | `wifi/sender/main.c` |

## How to pin it exactly later

This needs network access, once.

1. Clone upstream into a folder outside this repository:

   ```bash
   git clone https://github.com/opendroneid/opendroneid-core-c /tmp/odid-upstream
   ```

2. Find the commits whose files match every hash above. From this repository's top folder:

   ```bash
   git ls-files -s third_party/opendroneid-core-c | grep -v PROVENANCE.md | awk '{print $2, $4}' | sed 's# third_party/opendroneid-core-c/# #' | sort > /tmp/odid-ours.txt
   ```

   ```bash
   for c in $(git -C /tmp/odid-upstream rev-list --all); do git -C /tmp/odid-upstream ls-tree -r "$c" | awk '$2 == "blob" {print $3, $4}' | sort | comm -23 /tmp/odid-ours.txt - | grep -q . || echo "$c"; done
   ```

   Every commit printed holds all of these files byte for byte, so any of them is a faithful
   source; record the range, and the newest one dated before 2026-11-09 as the pin. If none is
   printed, the copy was edited: run the `comm` line for the nearest commit without `grep -q` to
   see which files differ.
3. Compare the file lists too (`git -C /tmp/odid-upstream ls-tree -r --name-only <commit>`), to see
   what upstream has that the copy left out.
4. Record the commit here, and in
   [Credits and licences](https://github.com/holdTheDoorHoid/rotorscope/blob/main/docs/project/credits.md).
