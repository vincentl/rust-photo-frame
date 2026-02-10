# Software Install Notes

This appendix captures advanced installer behavior and edge cases for `docs/software.md`.

## Rust toolchain behavior

- The system stage installs a minimal Rust toolchain under `/usr/local/cargo` with rustup state in `/usr/local/rustup`.
- The app deploy step prefers those system proxies and defaults `RUSTUP_HOME` to `/usr/local/rustup` so a default toolchain is available without per-user initialization.
- `CARGO_HOME` remains per-user for writable registries and caches.
- If you encounter `rustup could not choose a version of cargo to run` during build:
  - Ensure system stage has been run: `sudo ./setup/system/install.sh`
  - Or export: `RUSTUP_HOME=/usr/local/rustup`
  - Avoid overriding `RUSTUP_HOME` to `~/.rustup` unless you initialize a per-user toolchain (`rustup default stable`).

## Build memory and OOM mitigation

The installer auto-tunes Cargo job count on lower-memory Pis. If build workers are killed (`signal: 9`), cap jobs explicitly:

```bash
CARGO_BUILD_JOBS=2 ./setup/install-all.sh
# or
CARGO_BUILD_JOBS=2 ./setup/application/deploy.sh
```

Also verify swap after system provisioning:

```bash
swapon --show
```

Expect a `zram0` entry.

## Filesystem roles

- `/opt/photo-frame`: read-only runtime artifacts (binaries, unit templates, stock config files).
- `/var/lib/photo-frame`: writable runtime state (logs, hotspot artifacts, synced media, operational state files).
- `/etc/photo-frame/config.yaml`: active system configuration.

This separation allows redeploys to refresh `/opt` without clobbering operator-managed runtime data under `/var/lib/photo-frame`.

## Deployment postcheck notes

The postcheck defers some systemd validation until kiosk provisioning exists. If this is the first application deploy, warnings about `greetd.service` and helper units can appear until provisioning is complete.

## Installer environment variables

Use these to customize installation behavior:

| Variable        | Default            | Notes |
| --------------- | ------------------ | ----- |
| `INSTALL_ROOT`  | `/opt/photo-frame` | Target installation prefix. |
| `SERVICE_USER`  | `kiosk`            | Service account that owns `/var/lib/photo-frame`. |
| `SERVICE_GROUP` | `kiosk` (or primary group for `SERVICE_USER`) | Group ownership paired with `SERVICE_USER`. |
| `CARGO_PROFILE` | `release`          | Cargo profile passed to `cargo build`. |
