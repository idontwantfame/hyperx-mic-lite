# HyperX Mic Lite

A lightweight Windows microphone controller intended to replace the parts of HyperX/NGENUITY-style software that matter for a USB mic.

Current scope:

- List Windows capture devices.
- Show the default communications microphone.
- Mute, unmute, or toggle mute.
- Set default microphone input gain from `0` to `100`.
- Native GUI with Audio and Lights tabs.
- Detect the QuadCast S lighting HID controller.

The repo includes two implementations:

- `src/main.rs`: native Rust CLI.
- `HyperXMicLite.ps1`: PowerShell fallback with a basic tray menu.

## Run Now

Rust CLI:

```powershell
cargo run -- status
cargo run -- list
cargo run -- toggle
cargo run -- volume 75
cargo run -- lighting-detect
cargo run -- gui
```

Configuration:

```powershell
cargo run -- config path
cargo run -- config dump
cargo run -- config export .\config-export.json
cargo run -- config import .\config-export.json
cargo run -- config validate
cargo run -- config reset
```

Logs:

```powershell
cargo run -- logs path
cargo run -- logs tail 80
```

PowerShell fallback:

```powershell
.\HyperXMicLite.ps1 list
.\HyperXMicLite.ps1 status
.\HyperXMicLite.ps1 toggle
.\HyperXMicLite.ps1 volume 75
.\HyperXMicLite.ps1 tray
```

## Build Native Rust Version

Install Rust with Rustup, then:

```powershell
cargo build --release
```

Use it:

```powershell
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe list
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe status
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe toggle
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe volume 75
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe lighting-detect
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe lighting-effect cycle 10
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe lighting-effect vu_meter 10
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe lighting-effect vu_meter forever
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe lighting-vu-test 65 3
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe lighting-effect wave 5 --packet-log
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe gui
```

Configuration:

```powershell
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe config path
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe config dump
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe config export .\config-export.json
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe config import .\config-export.json
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe config validate
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe config reset
```

Logs:

```powershell
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe logs path
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe logs tail 80
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe diagnostics export
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe eventlog status
```

Important lifecycle and failure events are also written to the Windows Application event log with provider `HyperXMicLite`. To make Event Viewer render friendly messages, register the source once from an elevated terminal:

```powershell
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe eventlog register
```

`service install` also registers this Event Viewer source automatically.

Diagnostics export creates a folder containing a manifest, redacted config, recent app log, service health, Core Audio device/status JSON, and HID report-size details for supported lighting interfaces. Use `--packet-log` on lighting commands when protocol work needs packet-level HID write logs.

Windows service:

```powershell
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe service install
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe service start
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe service status
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe service plan
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe service stop
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe service uninstall
```

Install/uninstall normally require an elevated terminal. The installed service auto-starts and currently restores configured microphone volume/mute state when `service.restore_on_startup` is enabled in the config. Windows services run in Session 0, so they do not show the GUI on your desktop. `service status` includes the Windows SCM state plus the app's last service health heartbeat when available.

Service ownership is intentionally narrow. The service owns boot-time restore, lifecycle/status, health heartbeat, and Event Viewer setup. The logged-in user session owns GUI rendering, per-user GUI startup, interactive lighting streams, and HID events used for UI refresh. Lighting loops, background HID policies, and tray handoff are left as explicit future candidates rather than hidden service behavior.

Per-user GUI startup:

```powershell
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe startup install
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe startup status
.\target\x86_64-pc-windows-gnu\release\hyperx-mic-lite.exe startup uninstall
```

The startup command does not require admin. It launches the GUI for the current Windows user at login.

## Limits

Use the release binary for normal testing. The Lights tab detects the QuadCast S HID controller and can apply Solid, Wave, Cycle, Pulse, Blink, Lightning, and VU Meter effects through the packet writer. GUI lighting streams keep running while the app is open, and CLI effects run forever when `forever` is passed. VU Meter uses smoothed attack/release levels with flame-style colors, and the live-state option writes green/red lighting from HID mute reports.

The Audio tab includes a persisted `Mute microphone when app starts` option.
