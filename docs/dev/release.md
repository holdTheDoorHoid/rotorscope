# Releases: build, check, sign

**Level: 🟡 Intermediate, then 🔴 Expert** · How a firmware release is built so that anyone can
build the same bytes again, how you check a release you downloaded, and what is still missing.

> [!IMPORTANT]
> As of 2026-11-19 there is **no published release**, and nothing is signed yet. The recipe below
> is **built, not yet tried** on hardware: the images it makes have been compared byte for byte
> between builds, but none has been flashed to a board. The P4 hub part of it has never been run.

## What a release is

One folder per commit, made by `tools/release-build.sh`:

| File | What it is |
|---|---|
| `gateway.bin`, `node-wifi.bin`, `node-ble.bin`, `node-sdr.bin`, `hub-p4.bin` | The **app image** for each board: the exact bytes the flashing tool writes to the board's app slot. No debug information and no paths from the builder's computer. |
| `rid-beacon-BENCH-ONLY.bin` | The bench test transmitter, which sends a fake drone. **Not for the field**: read [Law, safety and ethics](../start/law-and-ethics.md) first. |
| `elf/<name>.elf` | The program file each image was made from, with its debug information (for decoding a crash backtrace). The image records this file's SHA-256, so the two belong together. |
| `parts/<name>.txt`, `parts/<name>.cdx.json` | The **parts list** of each image: every crate, its version, licence and checksum, the binary radio libraries, and the code the repository patches or carries. The `.cdx.json` is the same in the CycloneDX format that supply-chain tools read. |
| `BUILD-INPUTS.txt` | What went in: the commit, the Rust and espflash versions, each lockfile's SHA-256, the build settings. |
| `SHA256SUMS` | The SHA-256 of every file above. |
| `SHA256SUMS.sig` | The signature over `SHA256SUMS` (**planned**; see [Signing](#signing)). |

The node images are the ESP-NOW builds a kit uses: node-wifi as `cargo fw24-now` builds it (one C5
image covers both bands; `band24` only sets the band it starts in), node-ble with ESP-NOW, node-sdr as
`cargo fw-now`. The wired and bus builds and the S3 builds are not part of a release.

## Check a release you downloaded

You need `sha256sum` (part of every Linux; on macOS `shasum -a 256`) and, once releases are signed,
the signature check from [Signing](#signing). In the release folder:

```bash
sha256sum -c SHA256SUMS
```

```text
BUILD-INPUTS.txt: OK
elf/gateway.elf: OK
gateway.bin: OK
...
```

Every line must say `OK`. A checksum only proves the files are the ones the list names; the
signature over the list is what proves the list came from the project.

## Build the same image yourself

This is how you check that a published image really comes from the published source, without
trusting whoever built it. You need a Linux computer with the repository's tools
([Set up your machine](setup.md)).

1. Install **exactly** the versions `BUILD-INPUTS.txt` names. For a release built with Rust 1.99.1
   and espflash 4.7.0:

   ```bash
   rustup toolchain install 1.99.1 --target riscv32imac-unknown-none-elf --target riscv32imafc-unknown-none-elf
   ```

   ```bash
   cargo install espflash --version 4.7.0 --locked
   ```

2. Get the commit the release names, and download every crate once (the build itself never uses
   the network). Do the fetch in each firmware folder you want to check:

   ```bash
   git clone https://github.com/holdTheDoorHoid/rotorscope.git && cd rotorscope
   ```

   ```bash
   git checkout <commit from BUILD-INPUTS.txt>
   ```

   ```bash
   (cd firmware/gateway && cargo fetch --locked)
   ```

3. For the P4 hub image only, prepare the patched esp-hal once (this downloads one pinned commit):

   ```bash
   tools/p4-prepare.sh
   ```

4. Build, with the toolchain from step 1:

   ```bash
   tools/release-build.sh --commit <commit> --toolchain 1.99.1 --out ../my-build
   ```

   `--only gateway,node-wifi` builds just those images.

5. Compare your list with the published one:

   ```bash
   diff ../my-build/SHA256SUMS <published release folder>/SHA256SUMS
   ```

   No output means every file you built is byte for byte the published one. With `--only`, only
   your images' lines and `BUILD-INPUTS.txt` (whose image list then differs) are compared.

If a line differs, look at `BUILD-INPUTS.txt` first: a different Rust, cargo or espflash version
gives different bytes, and that file says which.

## Make a release (the owner)

1. Commit everything; the script refuses a tree with changes or untracked files.
2. Have every crate in the local cargo registry (build each firmware once, or `cargo fetch
   --locked` in each folder) and, for the hub, the patched esp-hal (`tools/p4-prepare.sh`).
3. Build:

   ```bash
   tools/release-build.sh
   ```

   It writes `release-out/rotorscope-<commit>/` (kept out of git), prints `SHA256SUMS`, and ends
   with the **SIGNING STEP** it leaves to you.
4. Sign `SHA256SUMS` ([Signing](#signing)), producing `SHA256SUMS.sig` in the same folder.
5. Ask a second person (or a second machine) to [build the same image](#build-the-same-image-yourself)
   and compare before publishing.

`tools/release-build.sh --self-test` checks the script's own decisions (dirty trees, bench builds,
key tags, builder details in an image, cargo settings outside the repository, the fixed build path,
that each image is built the way the repository builds it, the P4 checkout, the checksum file and the
parts-list script) without building anything.

It refuses to ship:

- a **bench build**: an image carrying a `bench-build:` tag other than `none`
  ([Bench builds](../BENCH.md#bench-builds-2026-11-18-compile-checked-only)). The build runs with an
  empty environment, so `ROTORSCOPE_BENCH` is never set and a bench feature stops the build before
  that;
- an image with a `link-key:` tag (images never hold a key; a kit's keys are written to each board
  separately);
- an image or ELF that holds the builder's home folder, user name or computer name;
- a build with cargo settings outside the repository that change what is built (rustflags, a
  profile, `[env]`, `[patch]`, `[unstable]` in the cargo home's `config.toml`, or a `.cargo` folder
  above the build folder).

## How the bytes are made the same everywhere

**🔴 Expert from here on.** If you only want to check or reproduce a release, you can stop here.

Three things made two builds of one commit differ. Each is now taken out, and
`tools/check-image-identity.sh` builds the gateway and node-wifi from two checkouts in different
folders and compares them:

| What differed | Why | What takes it out |
|---|---|---|
| The build time in the app descriptor | `esp-bootloader-esp-idf` stamps it | `SOURCE_DATE_EPOCH` = the commit's time |
| Paths in panic messages and debug info | rustc writes absolute paths for crates outside each firmware's own workspace (`crates/*`, the registry), and the folder it ran in | `--remap-path-prefix` for the home folder, rustup home, cargo home and checkout (`tools/repro-env.sh`) |
| The layout of the whole image | cargo mixes each `crates/*` crate's absolute location into its symbol names (`-C metadata`), and the linker then orders code differently | build through **one fixed path**, `/tmp/rotorscope-repro`, a link to the checkout (`repro_link` in `tools/repro-env.sh`); cargo is given the manifest through it and keeps that path |

Measured 2026-11-19 with Rust 1.99.1 and espflash 4.7.0, building commit `bfc1352`:

| Comparison | Result |
|---|---|
| Gateway (app image 612,768 bytes), two checkouts in different folders, path remapping only (the method before this page) | 44,724 bytes differ; in a second pair of folders 407 bytes. Node-wifi: 4,666 bytes |
| Gateway, both built through the fixed path, the real folder not yet remapped | 65 bytes differ, all of them the ELF's SHA-256 (see below): the debug info named the real folder. The code is identical |
| Gateway and node-wifi, both through the fixed path with every host path remapped (the release method) | app images and ELFs identical |
| Node-wifi, the same, one of the two builds with an extra remapping rule in its rustflags (as a different home folder gives) | app images and ELFs identical: rustflags do not change symbol names |

What `espflash` writes from an ELF that carries debug information, measured on the gateway: only
the loadable segments. Stripping the debug information (`llvm-objcopy --strip-debug`), or all
symbols, left the image's 612,768 bytes the same apart from **65 bytes**: espflash writes the SHA-256
of the whole ELF file into the app descriptor (32 bytes at offset `0xb0`), and that changes the
image's own SHA-256 digest (32) and checksum (1) at the end. So the debug information is never
flashed, but it decides 65 bytes of the image. That is why the release keeps one ELF per image, with
debug information and no host paths, publishes it beside the image, and makes the image from it.

Why not build without debug information instead: telling cargo to strip at link time
(`strip = "debuginfo"`) changed the code itself (different symbol names, a different layout), so a
release image would no longer match a developer's `cargo fw` build address for address, and a crash
backtrace from a fielded board could not be decoded with a normal build. The profiles in each
firmware's `Cargo.toml` are unchanged.

The search for builder details looks for the home folder path, the user name and the computer name
(each taken from the environment when the script runs, never written down) and the build folders, in
both the image and the ELF. On 2026-11-19 none was found in the gateway or node-wifi output.

## Limits

- **Exact tool versions.** The same commit gives the same bytes only with the same rustc, cargo and
  espflash versions; `BUILD-INPUTS.txt` names them. A newer Rust gives a different image.
- **Linux, x86-64, checked.** Two builders on other systems (macOS, an ARM computer) have not been
  compared. The script needs `flock`, which macOS lacks.
- **One build at a time per computer.** The fixed path `/tmp/rotorscope-repro` is shared; the script
  takes a lock and waits for any other build using it.
- **The image header** carries espflash's default flash mode, size and speed. Whether `espflash
  flash` changes those header bytes for a board whose flash it detects as a different size has
  **not been checked**; the published image is the one `espflash save-image` makes.
- **The bootloader and partition table** are not part of a release; the flashing steps in
  [Build and flash](../get-going/kit-flash.md) supply them.
- **The parts list** covers crates, the binary libraries inside them, the patched crates and the
  vendored C library. It does not list the Rust core library that comes with the toolchain;
  `BUILD-INPUTS.txt` names the toolchain instead.
- **Nothing proves who built it** until `SHA256SUMS` is signed.
- **The P4 hub part** of the script has never run (the bench machine that wrote it had no patched
  esp-hal checkout to build from). It fails clearly without one.

## Signing

Not set up. These are the options for signing `SHA256SUMS`, for the owner to choose from. None
needs a change to the build; the signature is one more file beside the sums.

| Option | What the owner needs | What a buyer runs | Notes |
|---|---|---|---|
| **SSH signature** (`ssh-keygen -Y sign`) | An SSH key kept for releases (a hardware key such as a FIDO token works), and its public half published in the repository as an "allowed signers" file | `ssh-keygen -Y verify`, part of OpenSSH 8.1 and later, already on Linux, macOS and Windows 10 and later | Nothing to install on either side. Recommended. |
| **minisign** | The minisign tool and a key pair made with it | minisign, installed first | Small and simple, but one more tool for every buyer. |
| **GPG** | A GPG key, kept and renewed | gpg, plus importing and trusting the key | Familiar to Linux distributions; the most steps for a buyer. |
| **GitHub artifact attestations** | The release built by a GitHub Actions workflow | `gh attestation verify <file> --repo holdTheDoorHoid/rotorscope` | Says "GitHub's runner built this from that commit", not "the owner approved it". Useful as a second, independent build alongside one of the above. |

With SSH signing, the owner's signing step and the buyer's check would look like this. The key file,
the identity and the signers file are placeholders until the owner chooses them:

```bash
ssh-keygen -Y sign -f ~/.ssh/rotorscope-release -n rotorscope-release SHA256SUMS
```

```bash
ssh-keygen -Y verify -f allowed_signers -I release@rotorscope.invalid -n rotorscope-release -s SHA256SUMS.sig <SHA256SUMS
```

The buyer takes `allowed_signers` from the repository, not from the release folder: a signer list
that travels with the files it vouches for proves nothing.

## Where this lives in the code

- `tools/release-build.sh`: the recipe (`--help`, `--self-test`)
- `tools/release-parts.py`: the parts list and CycloneDX file (`--self-test`)
- `tools/repro-env.sh`: path remapping, `SOURCE_DATE_EPOCH`, the fixed build path, the search for builder details
- `tools/check-image-identity.sh`: two checkouts, two folders, one image
- `tools/check-wifi-blobs.sh`: the radio libraries and the patched esp-radio, checked before every release build
- `firmware/patches/`: the patched esp-radio and the P4's esp-hal patches

**Next:** [Crate reference](crates.md)
