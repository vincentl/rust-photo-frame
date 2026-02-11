# Photo Frame Public Release Roadmap (Maker-Focused)

## Release Goal
A technically capable maker can:
1. Flash Raspberry Pi OS.
2. Run one install path.
3. Load photos.
4. Wake/sleep and recover Wi-Fi reliably.
5. Operate the frame remotely with documented, optional remote-admin tooling.

## Scope
### In Scope (v1 release)
- Install reliability and first-run success.
- Runtime/service consistency.
- Wi-Fi recovery reliability and diagnostics.
- Operator documentation completeness.
- Optional sync setup guidance (not mandatory for first run).

### Out of Scope (v1 release)
- Bundling VPN services in installer.
- New local configuration web UI.
- Additional rendering effects/features unrelated to install/ops reliability.
- Fabrication expansion beyond existing maker docs.

## Milestone 1 - Clean Install Reliability (Release Blocker)
- [x] Standardize runtime namespace to `photoframe` for control socket/runtime directory usage.
- [x] Normalize all runtime socket/runtime-directory references to `/run/photoframe/...`.
- [x] Ensure deployment starts a kiosk session that creates the control socket without requiring manual reboot.
- [x] Add explicit post-deploy readiness check for control socket presence.
- [x] Fix `setup/tools/verify.sh` seatd checks to avoid false warnings on service-only seatd installs.
- [x] Update install docs with clear operator flow:
- [x] When logout/login is required to pick up group membership.
- [x] How to add photos to `local/` and `cloud/`.
- [x] `scp` examples including custom key usage (`-i ~/.ssh/<key>`).

Exit criteria:
1. Fresh Pi install completes with `./setup/install-all.sh`.
2. `./setup/tools/verify.sh` returns no misleading warnings on supported baseline.
3. Control socket exists immediately after install flow completes (no reboot workaround).

## Milestone 2 - Wi-Fi Recovery Reliability (Release Blocker)
- [x] Harden `tests/run_wifi_recovery.sh` preflight:
- [x] Validate monitored interface from active wifi-manager config.
- [x] Confirm interface has an active infrastructure connection before fault injection.
- [x] Abort early with clear remediation if preconditions fail.
- [x] Make `developer/suspend-wifi.sh` fault injection deterministic for acceptance testing.
- [x] Improve recovery test output to show why transition to `RecoveryHotspotActive` did not happen.
- [x] Fix `print-status.sh` active connection reporting so operators can trust status snapshots.
- [x] Add triage appendix for "recovery test hangs" in operator docs.

Exit criteria:
1. Wi-Fi recovery acceptance passes on fresh install in documented supported scenario.
2. Failure mode output is actionable without code reading.
3. Status tools correctly report active Wi-Fi state.

## Milestone 3 - Operations Completeness (Must Ship)
- [x] Publish remote administration guidance (optional) covering:
- [x] SSH hardening baseline.
- [x] Tailscale option.
- [x] Raspberry Pi Connect option.
- [x] Recovery path if remote access is lost.
- [x] Document sync service configuration:
- [x] How to enable `photoframe-sync` safely.
- [x] Required env file variables and examples.
- [x] Expected timer/service statuses.
- [x] Decide and implement sync default posture:
- [x] Disabled by default until configured, or
- [ ] Enabled with no-noise behavior when unconfigured.
- [x] Ensure SOP includes one canonical "daily health check" sequence and expected outputs.

Exit criteria:
1. Operator can configure optional remote admin and sync by following docs only.
2. Sync does not generate confusing default-state noise.
3. SOP and software guide are consistent and complete for day-0/day-2 operations.

## Milestone 4 - Release Candidate Validation
- [x] Run full link checker for docs and fix all broken anchors/paths.
- [ ] Execute smoke, daily, and wifi-recovery test scripts on a clean image.
- [ ] Capture and archive log bundle from validation run.
- [ ] Perform at least two independent clean-install rehearsals using docs only.
- [ ] Convert validation findings into final doc/script fixes before tagging release.

Exit criteria:
1. Two successful clean-install runs with no undocumented workaround.
2. Test scripts complete in expected path and produce expected evidence.
3. Documentation is sufficient for a new maker with software experience.

## Deferred Backlog (Post-v1)
- [ ] Config web UI.
- [ ] Additional rendering experiments/presets.
- [ ] Curated starter background image pack.
- [ ] Expanded hardware/fabrication productionization work.
