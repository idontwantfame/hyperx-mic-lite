# USBPcap capture: 6-live-muted-lighting

Capture file: `D:\6-live-muted-lighting.pcapng`

This capture was taken while changing the old app's live/muted lighting
behavior and muting/unmuting the microphone.

## Devices

- Device address `9`: `0951:171d`, USB audio function.
- Device address `10`: `0951:171f`, HID lighting/control function.

All lighting/config traffic below is on device address `10`.

## Physical mute reports

The capture contains normal physical mute input reports:

```text
05 10 00 00 00 00 00 00
05 10 01 00 00 00 00 00
```

It also includes `05 08 ff 00 00 00 00 00` reports while the old app is
interacting with lighting state.

## Live lighting stream

The old app continues to stream ordinary lighting frames. Its dominant frame in
this capture was:

```text
81 f2 00 00 81 f2 00 00 ...
```

## Live/muted policy commands

Additional command payloads appeared while changing live/muted behavior:

```text
04 56 ... 01 ...
04 57 ... 01 ...
00 00 ... ff 00 00 00 01 ... aa 55
00 00 ... ff 00 00 00 00 ... aa 55
04 02 ...
04 53 ... 01 ...
04 23 ... 01 ...
08 00 ... 28 01 00 aa 55
```

Interpretation: live/muted lighting likely has a persisted device policy bit in
the sentinel-style command payloads. Our current app's live/muted behavior is a
session-side policy that writes green/red frames when mute events arrive. That
is safer and avoids writing device memory implicitly.

## Implementation impact

- Do not auto-save live/muted lighting policy during normal Apply.
- Keep the current session-side live/muted behavior unless an explicit
  persistent-save action is requested.
