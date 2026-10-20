# Linux VM on the Mac (Lima)

A Debian 13 (trixie) arm64 VM that gives an Apple Silicon Mac the same
architecture and glibc as the Pi 5. It exists for three jobs, in this order:

1. **Linux `cargo test` / `cargo clippy`.** The Mac only compiles the
   `cfg(not(target_os = "linux"))` side of the code and, with
   `cargo check --target aarch64-unknown-linux-gnu`, type-checks the Linux side.
   The VM actually builds and runs it.
2. **Setup smoke test.** Run the real `setup/system` and `setup/application`
   modules against a disposable machine: the `kiosk` user and groups, the
   `/opt/photoframe` tree, every systemd unit, `setup/tools/verify.sh`, the
   journal. No Pi involved.
3. **Build off the Pi.** Compile the release binaries in the VM, stage them as
   `deploy.sh` would, and ship them to the Pi over rsync instead of building
   there. Same architecture and glibc, so the binaries run unchanged.

It is **not** a way to see how the frame behaves; see
[What the VM cannot tell you](#what-the-vm-cannot-tell-you).

| File | Purpose |
|------|---------|
| [lima/photoframe.yaml](lima/photoframe.yaml) | The VM (vz backend). Use this one. |
| [lima/photoframe-qemu.yaml](lima/photoframe-qemu.yaml) | **Experimental** qemu variant for a KMS smoke test. Optional. |
| [lima/smoke-setup.sh](lima/smoke-setup.sh) | Setup smoke test (runs inside the VM). |
| [lima/stage-to-pi.sh](lima/stage-to-pi.sh) | Build + stage + rsync to a Pi (runs inside the VM). |
| [lima/common.sh](lima/common.sh) | Shared helpers for the two scripts. |

The template was written against Lima 2.2 and passes `limactl validate`;
it requires 2.0 or newer (`brew install lima`).

---

## 1. Create the VM

From the repo root (or any git worktree of it, anywhere under your home directory):

```bash
limactl start --name=photoframe developer/lima/photoframe.yaml
```

The first start downloads the Debian image (about 335 MB, cached by Lima afterwards) and
provisions the VM (apt packages, rustup). Measured: 1.5 minutes with the image already
cached, plus the download time on a cold cache. After that:

```bash
limactl stop photoframe        # free the CPU and RAM
limactl start photoframe
limactl delete photoframe      # throw it away
```

What the template sets up:

- **6 CPUs, 8 GiB RAM, 60 GiB disk**, vz backend. Edit the top of the YAML if
  your Mac is busy; the release build is the only heavy workload.
- **Your home directory is mounted read-only** at the same path inside the VM,
  so `cd <repo> && limactl shell photoframe <cmd>` runs against your working
  tree (uncommitted edits included), worktrees too. Read-only means nothing in
  the VM can modify the Mac checkout.
- **Build output is VM-local.** `CARGO_TARGET_DIR=/var/cache/photoframe-build/target`,
  and the cargo registry lives in the VM user's `~/.cargo`. Nothing is written
  to the macOS mount, which would be slow and would mix Linux artifacts into your
  Mac `target/`.
- **`RUSTFLAGS=-Ctarget-cpu=cortex-a76 -Ctarget-feature=-aes,-sha2`** for every build in the
  VM. `10-build.sh` otherwise defaults to `target-cpu=native`, which in the VM means an
  Apple core and can emit instructions the Pi lacks (the symptom is `SIGILL` on the Pi).
  Plain `cortex-a76` is not enough: rustc's definition of that core also enables `aes`
  and `sha2` (the Armv8 crypto extension), and the Pi 5's BCM2712 does not implement it on
  every stepping (reports of `/proc/cpuinfo` differ, and nothing in the VM can tell which
  you have: Apple cores have crypto). `-aes,-sha2` removes exactly those two and leaves
  `crc`, `lse`, `dotprod`, `fp16`, `rcpc`, `rdm`, which every Pi 5 reports. `stage-to-pi.sh`
  refuses `native`, and `cortex-a76` without the two minuses. Do not override this for
  anything you intend to ship.
- **Rust** installed system-wide under `/usr/local/{rustup,cargo}`, stable, with
  clippy and rustfmt, like `setup/system/modules/20-rust.sh`.
- **apt packages** from `setup/system/modules/10-apt-packages.sh`, *minus*:
  `sway`, `swaybg`, `swayidle`, `swaylock`, `seatd`, `libgl1-mesa-dri` (the
  compositor stack), `kmscube` and `network-manager`. Building and testing do not
  need them. The smoke test does, and gets them by running the real system
  installer (below). Kept: the build libraries (`libudev-dev`, `libssl-dev`,
  `libclang-dev`, `libdrm2`, `libgbm1`, `libinput10`, `libwayland-client0`) and
  `mesa-vulkan-drivers` + `vulkan-tools`, which give you lavapipe, a software Vulkan
  device, for any test that creates a wgpu adapter.
- **No containerd.** Lima turns on rootless containerd by default (nerdctl-full download, ~60
  binaries in `/usr/local/bin`, `limactl start` waits on it); the template switches it off.
- **ssh agent forwarding** so the VM can rsync to the Pi with your Mac's keys
  (nothing is copied into the VM).

## 2. Linux tests and clippy

Run from the repo root on the Mac; Lima starts the shell in the same directory inside the VM.

```bash
limactl shell photoframe -- cargo test --workspace
limactl shell photoframe -- cargo clippy --workspace --all-targets -- -D warnings
limactl shell photoframe -- cargo fmt --all --check
```

These are the same checks as [testing.md](testing.md#automated-checks-no-pi-required),
now with the Linux `cfg`s compiled in. The first run builds the whole dependency
tree; later runs are incremental. To confirm what you are running on:

```bash
limactl shell photoframe -- bash -c '. /etc/os-release; uname -m; echo $VERSION_CODENAME; ldd --version | head -1; rustc --version'
```

**Toolchain drift.** The VM installs *current* stable (rustc 1.99.1 on 2026-10-19), which can be
newer than the Mac's, and a newer clippy has more lints, so `-D warnings` can fail in the VM on
code the Mac passes. To tell "new lint" from "Linux-only problem", also run the older toolchain
(here the Mac's 1.89.0):

```bash
limactl shell photoframe -- sudo env RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo /usr/local/cargo/bin/rustup toolchain install 1.89.0 --profile minimal -c clippy
limactl shell photoframe -- cargo +1.89.0 clippy --workspace --all-targets -- -D warnings
```

**Disk.** `cargo test` plus clippy on two toolchains leaves about 11 GB of debug output in the VM
(release is about 1 GB), and the VM's disk image grows on the Mac by the same amount. Give
the Mac that much room, and reclaim it with:

```bash
limactl shell photoframe -- bash -c 'rm -rf $CARGO_TARGET_DIR/debug && sudo fstrim -v /'
limactl stop photoframe   # the image file shrinks on the Mac once the VM is stopped
```

If a test needs to write inside the source tree it will fail with
`Read-only file system` (the mount is read-only). Run it from the VM-local copy
instead, which is what the scripts below use:

```bash
limactl shell photoframe -- bash -c 'source developer/lima/common.sh && lima_sync_src && cd "$SRC_COPY" && cargo test --workspace'
```

## 3. Setup smoke test

`smoke-setup.sh` runs the real thing, in this order:

1. copies the working tree into the VM (`/var/cache/photoframe-build/src`), because
   the application modules `chmod` and write inside the repo and the mount is read-only;
2. `sudo setup/system/install.sh`: all system modules, which also installs the
   compositor/greetd set (`sway*`, `seatd`, `greetd`, `kmscube`, `network-manager`), creates
   `kiosk` with `render video input seat`, installs polkit rules and units, sets persistent
   journald, masks `getty@tty1`. NetworkManager is told to leave the VM's own interface
   alone first, so it cannot cut your ssh session;
3. `setup/application/deploy.sh`: `05-service-user`, `10-build`, `20-stage`, `30-install`,
   `35-fonts` (downloads Macondo from GitHub), `45-activate-services`, `50-postcheck`;
4. checks the kiosk user/groups, the `/opt/photoframe` tree, and that the staged binary is aarch64;
5. `systemd-analyze verify` on every `*.service` / `*.timer` under `assets/systemd/` and `setup/`
   (whichever branch is checked out, so the units from other branches get checked too);
6. prints enabled/active state and `systemctl --failed`;
7. runs `setup/tools/verify.sh`;
8. shows the journal for the photoframe units and the `photoframe` tag.

It reconfigures the machine heavily (zram swap, journald, logind, getty, display manager),
so **run it in a clone**, which keeps the VM from section 1 clean for tests and builds:

```bash
limactl stop photoframe
limactl clone --start photoframe photoframe-smoke
limactl shell photoframe-smoke -- developer/lima/smoke-setup.sh
```

Re-run it in the same clone to test idempotence (the setup is meant to be re-runnable).
Start over with:

```bash
limactl delete --force photoframe-smoke
limactl start photoframe
```

**What "pass" means here.** The script ends with `N hard failure(s), M expected-in-VM item(s)`
and exits non-zero only for hard failures. `greetd.service` cannot bring up a compositor
without the Pi's GPU and display, so these are expected and listed separately, not counted
as failures: `50-postcheck` failing on greetd or the control socket, and `verify.sh` reporting `greetd.service not active` (greetd restart-loops, so
verify.sh may just as well catch it "active"; on the first run it did). `verify.sh` runs under
`sudo`: as the VM user it cannot see the 0750 `/var/lib/photoframe` or the 0660 `sync.env`
and prints phantom warnings. Every other module, every unit verification, and
every other `verify.sh` error is a real finding. `photoframe-wifi-manager.service` may
also restart-loop in the journal (no Wi-Fi hardware); that is worth a look but is not
scripted as a failure.

Logs land in `/var/cache/photoframe-build/smoke/` inside the VM.

## 4. Build in the VM, stage to the Pi

`deploy.sh` normally builds on the Pi (`10-build`), stages (`20-stage`), then installs and
restarts services (`30`, `35`, `45`, `50`). With the VM you build and stage there and let
the Pi only do the install half:

```bash
# once per Mac login: make the key available to the VM
ssh-add

# build release in the VM, stage, rsync to the Pi
limactl shell photoframe -- developer/lima/stage-to-pi.sh pi@picframe.local
```

To build and stage without a Pi (no ssh agent, no rsync; the tree is left in
`/var/cache/photoframe-build/stage-root/stage` in the VM):

```bash
limactl shell photoframe -- developer/lima/stage-to-pi.sh --stage-only
```

That script copies the working tree into the VM, runs `10-build.sh` and `20-stage.sh`
unchanged (the staged tree has the same `bin etc docs share systemd` layout as on the Pi),
checks that `photoframe`, `wifi-manager` and `buttond` are aarch64 ELFs with no unresolved
shared libraries, and rsyncs the stage directory to `~/photoframe-stage/stage/` on the Pi.
It then prints the Pi-side command; or add `--deploy` to run it for you:

```bash
limactl shell photoframe -- developer/lima/stage-to-pi.sh --deploy pi@picframe.local
```

The Pi-side command is:

```bash
cd ~/photoframe && STAGE_ROOT="$HOME/photoframe-stage" ./setup/application/deploy.sh --skip-build
```

**How this interacts with `deploy.sh`.** `deploy.sh` gained one flag, `--skip-build`,
which skips `10-build.sh` and `20-stage.sh` and fails early if
`${STAGE_ROOT}/stage/bin/photoframe` is missing. Everything else runs as usual against the
tree at `${STAGE_ROOT}/stage` (`STAGE_ROOT` already existed): `/opt/photoframe` is updated,
`/etc/photoframe/config.yaml` and `/var/lib/photoframe/` are preserved, units are
refreshed, services restart, postcheck runs. Without the flag nothing changes. (`20-stage.sh`
also now honours `CARGO_TARGET_DIR`, defaulting to `<repo>/target` as before; the VM
relies on it.)

Things to know:

- The Pi still needs its checkout (it runs `deploy.sh` and the modules from there) and
  must be **at the same commit** as the tree you built from. The staged `config.yaml`,
  docs and unit files come from the VM's copy of your working tree; the install scripts
  that consume them come from the Pi's checkout. `git pull` on the Pi first.
- First-time Pi setup is unchanged: `sudo ./setup/system/install.sh` on the Pi still runs there.
- The Pi no longer needs to compile, so `CARGO_BUILD_JOBS` and the memory cap in `10-build.sh`
  are irrelevant for this path.
- The Pi must run Debian 13, matching the VM's glibc. After the first deploy, confirm on the
  Pi: `ldd /opt/photoframe/bin/photoframe | grep 'not found'` prints nothing.
- Override the Pi paths with `PI_REPO` and `PI_STAGE` (both relative to the Pi's home;
  defaults `photoframe` and `photoframe-stage`).

---

## KMS smoke test (experimental)

`claude/viewer-on-kms-1eefd2` adds a compositor-free viewer path (`photoframe-kms.service`,
`tasks/viewer/kms.rs`, `PHOTOFRAME_DISPLAY=kms`). The setup half of that is covered by
the smoke test above like any other unit: `systemd-analyze verify` picks up
`photoframe-kms.service` from `assets/systemd/` when that branch is checked out.

The qemu variant exists only to find out whether the *runtime* half can be smoke-tested:

```bash
limactl start --name=photoframe-qemu developer/lima/photoframe-qemu.yaml
limactl shell photoframe-qemu -- bash -c 'ls -l /dev/dri; sudo modetest -M virtio_gpu | head -40; vulkaninfo --summary'
limactl shell photoframe-qemu -- bash -c 'vulkaninfo 2>/dev/null | grep -E "VK_KHR_display|VK_EXT_acquire_drm_display|VK_EXT_physical_device_drm"'
```

**Result (2026-10-19, see "First run"): this does not work.** The qemu guest has a virtio-gpu PCI
device but no `/dev/dri` (Debian's *cloud* kernel is built without DRM), and the `grep` above
is a **false positive**: `VK_KHR_display` and `VK_EXT_acquire_drm_display` show up as *instance*
extensions from the Vulkan loader whatever the device is. The only device is `llvmpipe`
(`PHYSICAL_DEVICE_TYPE_CPU`) and it lacks the device extension `VK_EXT_physical_device_drm`, so
look for that one instead. The original expectation, kept for reference:

- A `/dev/dri/card0` may appear (virtio-gpu); whether it does under Lima's qemu setup is the first
  thing to check.
- The only Vulkan device is lavapipe, which does not implement `VK_KHR_display`, so the
  viewer's KMS present path most likely fails at surface creation rather than running. If the grep
  prints nothing, stop here: the most the VM can then show is that the unit starts, logs the
  failure, and respects `StartLimit*` and `Conflicts=greetd.service` ordering. That is a legitimate
  crash/ordering test, but it is not a KMS test.
- If the extensions *and* `VK_EXT_physical_device_drm` show up, check out the KMS branch, run setup with
  `PHOTOFRAME_DISPLAY=kms` as that branch documents, and read
  `journalctl -u photoframe-kms.service -t photoframe`.

Whatever it prints, it says nothing about v3d, scanout, or timing. If the grep comes back
empty, delete the VM and drop `photoframe-qemu.yaml`; it is not worth keeping.

---

## What the VM cannot tell you

Treat any VM result as "the code and scripts run on arm64 Debian 13", never as "it works on the Pi".

- **Frame cadence, pacing, or any performance number.** No `pre_present_notify`
  pacing signal, no vsync, no 4K60, no mailbox/FIFO behaviour, and the CPU is an Apple core,
  not a Cortex-A76. Do not time anything here and quote it.
- **GPU behaviour.** No v3d, no Pi Vulkan driver, no real swapchain, no compositor
  bandwidth effects. lavapipe is a CPU rasteriser with different limits, formats and bugs.
- **Display output.** No HDMI, no hotplug or EDID, no mode setting, no connector
  state, no panel, no DPMS or display sleep, no `swaymsg`/`wlr-randr` behaviour.
- **Boot and hardware.** No Pi firmware or `config.txt` (the boot-config steps are skipped),
  no EEPROM check, no GPIO or power button (`buttond` runs but has no input device), no real
  Wi-Fi radio, so the captive-portal recovery path is not exercised.
- **Kernel specifics.** The guest kernel is Debian's generic cloud kernel, not Raspberry
  Pi OS's; cgroup, zram, CMA and input-device behaviour can differ.
- **Compositor startup.** greetd/sway/`photoframe-session` are expected to fail without a DRM
  display, so a clean `deploy.sh` through `50-postcheck` is not something to look for here.

## First-run check

You cannot get this wrong by reading; run these once and look for three things.

1. `limactl shell photoframe -- bash -c 'uname -m; echo $CARGO_TARGET_DIR $RUSTFLAGS; cargo --version'`
   prints `aarch64`, `/var/cache/photoframe-build/target -Ctarget-cpu=cortex-a76 -Ctarget-feature=-aes,-sha2` and a cargo version.
   If the two variables are empty, `/etc/environment` did not reach the shell; use
   `limactl shell photoframe -- bash -lc '<cmd>'` and fix the template.
2. `cargo test --workspace` and clippy finish in the VM, and your Mac `target/` is untouched
   (the Linux artifacts are under `/var/cache/photoframe-build/target`).
3. `smoke-setup.sh` ends with **0 hard failures**, and the only expected-in-VM items are the greetd ones.
   Any `systemd-analyze verify` complaint, or a module before `50-postcheck` not completing,
   is a real finding.

---

## First run, 2026-10-19/20

Done by following this guide on branch `claude/lima-verify-2eeddf` (from `7ca4fce`) on an M-series
Mac (8 cores, 16 GiB) with Lima 2.2.0 and three other Lima VMs running. Debian image
`debian-13-genericcloud-arm64-20260712-2537` was already in Lima's cache, so no download time
is included.

| Step | Command | Measured |
|------|---------|----------|
| Create and provision | `limactl start --name=photoframe developer/lima/photoframe.yaml` | 1 m 30 s, every provision step succeeded (cloud-init `degraded done` is only Lima's own deprecated `ssh-authorized-keys`). Before the containerd fix, 40 s of that was waiting for rootless containerd. |
| Sanity | the First-run check commands below | `aarch64`, trixie, glibc 2.41, rustc/cargo 1.99.1, `CARGO_TARGET_DIR` and `RUSTFLAGS` set |
| `cargo test --workspace` | cold target dir | 3 m 01 s, 192 passed / 0 failed / 13 suites; 1 m 43 s when re-run from the VM-local copy |
| `cargo clippy --workspace --all-targets -- -D warnings` | rustc 1.99.1 | 50 s, **fails** (below) |
| same with `cargo +1.89.0` | | 28 s, **fails** with 2 errors (below) |
| `cargo fmt --all --check` | | fails, 4 hunks; identical on the Mac, not VM-related |
| Clone | `limactl stop photoframe && limactl clone --start photoframe photoframe-smoke` | 17 s |
| Smoke test | `limactl shell photoframe-smoke -- developer/lima/smoke-setup.sh` | 3 m 43 s first run (includes a release build), 2 m 55 s on the re-run; both ended `0 hard failure(s), 1 expected-in-VM item(s)` |
| Release build + stage | `limactl shell photoframe -- developer/lima/stage-to-pi.sh --stage-only` | 1 m 42 s (6 CPUs), then 0.2 s when nothing changed |
| qemu variant | `limactl start --name=photoframe-qemu --cpus 4 --memory 4 --disk 20 developer/lima/photoframe-qemu.yaml` | boots in 1 m 22 s; no `/dev/dri`; see KMS section |

**Clippy.** Eleven of the thirteen distinct errors under 1.99.1 are lints new since 1.89
(4 `derivable_impls` in `config.rs`, 6 `chunks_exact_to_as_chunks` in `fixed_image.rs` and
`scenes/mod.rs`, 1 `question_mark` in `manager.rs`); they would appear on the Mac with a current
toolchain too. Two are **Linux-only** and fail under 1.89.0 as well, because the Mac compiles
the `cfg(target_os = "linux")` block out: `needless_return` in `with_photo_app_id`
(`crates/photoframe/src/tasks/viewer.rs`) and `with_overlay_app_id`
(`crates/wifi-manager/src/overlay/ui.rs:68`). Not fixed here; this branch only changes the Lima
files. The same two are the only 1.89 clippy errors on `claude/viewer-on-kms-1eefd2` (`aed0dbf`),
whose `kms.rs`, `drm_display.rs` and `wake.rs` add no clippy warning on Linux; its
`cargo test --workspace` passes there too (260 passed, 0 failed).

**Staged layout.** `20-stage.sh` produced 60 entries: `bin/` {`photoframe`, `wifi-manager`, `buttond`,
`frametest`, `photoframe-cloud-sync`, `powerctl`, `print-status.sh`} at 0755 (the first four are
aarch64 ELFs, no unresolved libraries), `etc/photoframe/{config.yaml,sync.env}` and
`etc/wifi-manager.yaml` at 0644, `share/wordlist.txt` at 0644 plus an empty `share/backgrounds/`,
`docs/` (35 files plus `LICENSE`, all 0644), `systemd/` (4 units, 0644). That is exactly what the
module's rules give, and it is byte-for-byte the same file set and modes as the stage the
smoke run installed, and as `/opt/photoframe` after `30-install.sh`, apart from `/opt/photoframe`
itself (0755 root, the module creates it). Directories are 0775 owned by the build user, from the
VM user's umask 002, probably what an on-Pi build gives too (not checked on a Pi); `30-install.sh` copies the
stage's directory modes and owners as they are.

**Things the setup scripts do that are not Lima's fault** (found by the smoke test, not fixed):

- `sync_is_configured` reads `/etc/photoframe/sync.env` (0660 `kiosk:kiosk`) with `awk` as the
  deploying user, so `45-activate-services` and `50-postcheck` print `awk: cannot open ...
  (Permission denied)` and treat sync as unconfigured. On a Pi where the deploying user is not
  in `kiosk`, a configured sync would likewise not get its timer enabled.
- `verify.sh` has the same visibility problem when run unprivileged: phantom "library directory
  missing" warnings. Run it with `sudo`.
- `wifi-manager` logs `nmcli connection up pf-hotspot failed` every few seconds (no Wi-Fi
  radio): expected.

**What changed in the Lima files because of this run:** containerd disabled in the template;
`smoke-setup.sh` runs `verify.sh` under `sudo` and lists `seatd.service` (there is no
`seatd.socket`); `stage-to-pi.sh` gained `--stage-only` and checks `frametest` too.

**VM state left behind:** `photoframe` (clean base: toolchains 1.99.1 and 1.89.0, release build
cache, debug cache removed, trimmed) and `photoframe-smoke` (the clone with the full kiosk setup
installed, greetd restart-looping), both stopped. `limactl start photoframe` brings the base back;
`limactl delete --force photoframe photoframe-smoke` removes them. The qemu VM was deleted.
