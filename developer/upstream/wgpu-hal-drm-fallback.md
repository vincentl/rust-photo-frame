# Upstreaming the wgpu-hal DRM fallback

`wgpu-hal-drm-fallback.patch` is the one local change in `vendor/wgpu-hal`
(the `PHOTOFRAME PATCH` block in `src/vulkan/drm.rs`), rewritten as a
`git format-patch` against gfx-rs/wgpu `trunk` (checked at `3050396`, wgpu-hal
30.0.0). Merging it upstream lets us delete `vendor/wgpu-hal`, the
`[patch.crates-io]` entry and the `exclude` in the root `Cargo.toml`.

## What differs from the vendored block

`trunk`'s `create_surface_from_drm` has the same matching loop, but it
differs from 27.0.4 in three ways, so the patch is not a copy of the vendored
hunk:

- The loop consumes the device list (`for device in raw_devices`). The
  patch iterates by reference so the fallback can walk the list again.
- `acquire_drm_display_instance` is created after the loop; the patch
  creates it before the fallback, which needs it.
- The fallback is a short `find` instead of a loop with an `info!` log, and
  its comment is two lines. wgpu's `AGENTS.md` asks for the shortest complete
  comment.

`trunk` also gained `create_surface_from_drm_plane`, which calls
`create_surface_from_drm`, so the fallback covers it too. Behaviour is
unchanged wherever the existing lookup finds a device.

Checked in a `trunk` clone: `rustfmt --check`, `cargo check -p wgpu-hal
--features vulkan,drm --target aarch64-unknown-linux-gnu` and the same with
`clippy` (no warnings in `drm.rs`). **Not run:** the wgpu test suite (no test
exercises a DRM display fd), and the patched `trunk` code on a Pi. The
hardware test below ran the same logic against wgpu-hal 27.0.4.

## Read this before opening the PR: wgpu's AI policy

wgpu's `CONTRIBUTING.md` and `AGENTS.md` do not allow fully agentic PRs. The
contributor must understand and vouch for every change, and **the PR
description must be written by hand**. The body below is a draft of the
facts, written by an LLM, to rewrite in your own words rather than paste. The
template's "LLM Use?" section and the agent-disclosure checkboxes must be
filled in accurately; the draft does that.

wgpu does not changelog `wgpu-hal` changes unless `wgpu` users interface with
them directly (`AGENTS.md`), so the patch adds no `CHANGELOG.md` entry. A
maintainer may ask for one.

## PR title

```
[hal/vulkan] Fall back to the driver when no device matches a DRM fd
```

## PR body (draft; rewrite by hand)

```markdown
## Connections

None.

## Description

`create_surface_from_drm` picks the physical device whose
`VK_EXT_physical_device_drm` primary/render node equals the `st_rdev` of the
display fd it was given. That assumes the GPU and the display controller are
the same DRM device. On a Raspberry Pi 5 they are not: the GPU is `v3d`
(`/dev/dri/card0`, `renderD128`) and the display controller is `vc4`
(`/dev/dri/card1`). The Vulkan driver (Mesa v3dv) reports the GPU's nodes, so
nothing matches the `vc4` fd and surface creation fails with
`Failed to find suitable drm device`, although `VK_KHR_display` and
`VK_EXT_acquire_drm_display` are present and the driver can drive that
connector.

This adds a fallback: if no device matches by device number, use the first
physical device for which `vkGetDrmDisplayEXT` resolves the connector on the
fd. The driver is the authority on which of its devices can scan out to an fd
(the call is already used right after the lookup to get the display), so
asking it is more robust than comparing node numbers on hardware where
rendering and scanout are split. A device that matches by number is still
preferred, so nothing changes where the lookup already succeeds.

## Testing

Run on a Raspberry Pi 5 (Mesa 26.2.2, v3dv) with `frametest --drm`, a
fullscreen probe with no compositor that creates the surface through
`create_surface_from_drm`. Before: it stops with
`Failed to find suitable drm device`. After: the surface and swapchain are
created and the probe renders at 4K 60 Hz. That run used the
same change applied to wgpu-hal 27.0.4; on `trunk` I checked that it
compiles (`cargo check` and `clippy` for `aarch64-unknown-linux-gnu` with
`--features vulkan,drm`) but have not run it on a device. No unit test
covers this path since it needs a DRM display fd.

## Squash or Rebase?

Single commit; either.

## LLM Use?

<!-- Fill in honestly. Example: "Claude Code found and wrote the fix and the
     first draft of this text; I rewrote the description, read every line of
     the diff and ran the Pi test myself." -->

## Checklist

- [x] I self-reviewed and fully understand this PR.
- [ ] `CHANGELOG.md` entries for the user-facing effects of this change are present. (`wgpu-hal` internal; see `AGENTS.md`)
- [x] The PR is minimal, and doesn't make sense to land as multiple PRs.
- [x] Commits are logically scoped and individually reviewable.
- [x] The PR description has enough context to understand the motivation and solution implemented.
- [ ] (If applicable) WebGPU implementations built with `wgpu` may be affected behaviorally.
- [ ] (If applicable) Validation and feature gates are in place to confine behavioral changes.
- [ ] (If applicable) Tests demonstrate the validation and altered logic works.
- [x] This PR was automatically created by an Agent.   <!-- delete if you wrote and submitted it yourself -->
- [x] The description was automatically written by an Agent.   <!-- delete once you rewrite it by hand -->
```

## Commands

Set `GH_USER` to your GitHub login. Needs `gh` (logged in) and a Rust
toolchain with `aarch64-unknown-linux-gnu` for the optional check.

```bash
GH_USER=your-github-login
PATCH="$PWD/developer/upstream/wgpu-hal-drm-fallback.patch"   # run from the rust-photo-frame repo root
```

```bash
gh repo fork gfx-rs/wgpu --clone=false
```

```bash
cd "$(mktemp -d)" && git clone --filter=blob:none "https://github.com/$GH_USER/wgpu.git" && cd wgpu
```

```bash
git remote add upstream https://github.com/gfx-rs/wgpu.git && git fetch upstream trunk && git switch -c hal-vulkan-drm-fallback upstream/trunk
```

`--keep-non-patch` keeps the `[hal/vulkan]` prefix in the commit subject:

```bash
git am --keep-non-patch "$PATCH" && git log -1 --stat
```

If `trunk` has moved and `git am` reports a conflict, `git am --abort`, make
the change by hand (two hunks in `wgpu-hal/src/vulkan/drm.rs`) and commit with
the subject and body from the patch.

Optional local check (CI runs far more):

```bash
cargo fmt --check && cargo clippy -p wgpu-hal --features vulkan,drm --target aarch64-unknown-linux-gnu
```

```bash
git push -u origin hal-vulkan-drm-fallback
```

Write the body into `/tmp/pr-body.md` from the draft above, in your own
words, then:

```bash
gh pr create --repo gfx-rs/wgpu --base trunk --head "$GH_USER:hal-vulkan-drm-fallback" --title "[hal/vulkan] Fall back to the driver when no device matches a DRM fd" --body-file /tmp/pr-body.md
```

After it merges and a wgpu release containing it ships, bump `wgpu-hal`,
delete `vendor/wgpu-hal`, the `exclude` and the `[patch.crates-io]` entry in
the root `Cargo.toml`, and remove the paragraph about the vendored copy in
`docs/performance.md`.
