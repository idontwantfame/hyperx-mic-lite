# USBPcap captures: 6-9

Capture files:

- `D:\6-live-muted-lighting.pcapng`
- `D:\7-save-solid.pcapng`
- `D:\8-reset-default.pcapng`
- `D:\9-startup-open-only.pcapng`

These captures fill in the remaining NGENUITY behavior around live/muted
lighting, persistent lighting save, reset, and startup probing.

## Common devices

- Device address `9`: `0951:171d`, USB audio function.
- Device address `10`: `0951:171f`, HID lighting/control function.

All lighting/config traffic below is on device address `10`.

## Startup-only behavior

`9-startup-open-only.pcapng` shows the old app's initial lighting/config probe:

```text
04 58 ... 01 ...
04 56 ... 01 ...
04 56 ... 01 ...
04 57 ... 01 ...
00 00 ... ff 00 ... aa 55
04 02 ...
```

After that, the app starts normal live lighting streaming with:

```text
04 f2 ... 01 ...
81 f2 00 00 81 f2 00 00 ...
```

Interpretation: the `04 58`, `04 56`, `04 57`, sentinel, and `04 02`
packets are startup/config probing or setup, not ordinary frame rendering.

## Live/muted lighting

`6-live-muted-lighting.pcapng` contains normal physical mute reports:

```text
05 10 00 00 00 00 00 00
05 10 01 00 00 00 00 00
```

It also includes `05 08 ff 00 00 00 00 00` reports while the old app is
interacting with lighting state.

The old app continues to stream ordinary lighting frames. Its dominant frame in
this capture was:

```text
81 f2 00 00 81 f2 00 00 ...
```

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

## Save Solid

`7-save-solid.pcapng` confirms that `Save to Microphone` for Solid uses the
same command family seen in the Wave save capture.

Repeated save sequence:

```text
04 53 ... 01 ...
04 02 ...
04 23 ... 01 ...
08 00 ... 28 01 00 aa 55
```

The capture includes normal live frame writes before/after save, including:

```text
81 ff 97 00 81 ff 97 00 ...
81 ce ff 00 81 ce ff 00 ...
81 00 ff ff 81 00 ff ff ...
81 ff 00 00 81 ff 00 00 ...
```

Interpretation: `Save to Microphone` is effect-independent enough to expose as
one explicit action, but still should be treated as persistent storage and kept
separate from normal Apply.

## Reset Default

`8-reset-default.pcapng` mostly shows the same startup/config command family:

```text
04 56 ... 01 ...
04 57 ... 01 ...
00 00 ... ff 00 ... aa 55
04 02 ...
```

It did not reveal a clearly distinct, safe reset command beyond the generic
startup/config sequence. Avoid implementing reset from this capture alone.

## Implementation impact

- Keep normal Apply as live streaming only.
- Add `Save to Microphone` only as an explicit, experimental action.
- Do not add automatic persistent writes for live/muted behavior.
- Do not implement reset until there is a smaller isolated capture or a strong
  reason to support it.
- The most useful next code work is still:
  - 16-cell lighting renderer;
  - USB Audio Class writer for audio sliders/toggles;
  - optional explicit save action.
