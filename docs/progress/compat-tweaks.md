# Progress: compat-tweaks (2026-11-17)

Goal: visible, opt-in, reversible, journaled compatibility options (spec/local 80 §11,
spec/remote 50 §6 / 30 §10, spec/local 10 §8.8): Windows Update deferral + active hours,
fast startup off, device-install deny classes, Defender exclusions for our folders /
processes, USB dock wizard, class default descriptors switch (more classes), per-seat
named-object isolation + Steam per seat on local seats, Steam proxy-DLL report, DPI
readback warning, per-seat environment variables, "Deny log on locally" seat option,
manager README fix.

## Done (uncommitted in worktree)
- seat-core: config (Tweaks.fast_startup_off / windows_update / device_install_deny /
  defender_exclusions, SeatConfig.environment / deny_local_logon, settings.apps), validation
  (`compat_checks`), more DeviceClassTarget classes (seat-media classdefault)
- seat-compat: ops extensions, journal Change variants, tweaks WU + deny classes,
  defender.rs, rights.rs, user_env.rs, steam.rs, win_extra.rs (tests green)

## In progress
- seat-shim-core objects + module hooks; service wiring; proto; agent DPI; manager UI; docs

## Verify
- gates listed in the task (cargo test / clippy x2, manager test/check/build, shim, installer)
