# TODO

## Diagnostics And Reliability

- [x] Add structured app logging for startup, shutdown, device detection, HID events, audio changes, lighting writes, warnings, and errors.
- [x] Add Windows Event Viewer logging for important lifecycle and failure events.
- Register a proper Event Viewer message source during service/app install so events render friendly messages instead of raw provider records.
- [x] Add crash/panic capture with a useful report path and enough context to debug failures.
- Add a CLI command to export a diagnostics bundle with logs, device info, HID report sizes, app version, and recent settings.
- Add a debug/verbose flag for packet-level HID logging during protocol work.

## Configuration Management

- [x] Add a versioned config file for audio defaults, lighting presets, UI preferences, service settings, and device IDs.
- [x] Add CLI commands to dump, export, import, validate, and reset configuration.
- [x] Add config backup/restore behavior before risky imports or migrations.
- [x] Add config schema/version migration so older settings can be upgraded safely.
- Add redaction for diagnostics exports where config may contain local paths or device IDs.

## Background Service

- [x] Add CLI commands to install, uninstall, start, stop, and query a Windows service.
- Define what the service owns: startup restore, lighting loop/effects, HID monitoring, and optional tray/GUI handoff.
- [x] Add clear permission/admin handling for service installation.
- Add service logs and health/status reporting.

## UI Polish

- Smooth the input level meter with attack/release timing so it feels less jumpy.
- Make the GUI more polished: spacing, typography, icons, better controls, empty/error states, and visual consistency.
- Add a proper tray experience with mute state, current pattern, quick lighting presets, and open/exit actions.
- Persist window size, selected tab, last colors, brightness, speed, and preferred defaults.
- Add clearer device status indicators for disconnected, wrong device, unsupported feature, and write failure states.

## Lighting And Device Protocol

- [x] Promote `lighting-solid` into the GUI Apply button.
- [x] Add packet builders for Wave, Cycle, Pulse, Blink, Lightning, and VU Meter.
- Smooth the VU Meter lighting effect with attack/release timing and better bottom-to-top flame gradients(need to be flame colour too).
- Add live/muted lighting behavior that follows HID mute reports.
- Do a guided USBPcap + Wireshark capture session together to inspect what the original HyperX software sends for mic monitoring, headphone volume, lighting effects, and any hidden gain/dial controls.
