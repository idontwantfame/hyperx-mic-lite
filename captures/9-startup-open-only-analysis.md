# USBPcap capture: 9-startup-open-only

Capture file: `D:\9-startup-open-only.pcapng`

This capture was taken by starting USBPcap before opening the old app, then
waiting for the device view to settle without changing settings.

## Devices

- Device address `9`: `0951:171d`, USB audio function.
- Device address `10`: `0951:171f`, HID lighting/control function.

All lighting/config traffic below is on device address `10`.

## Startup/config probe

The old app's initial lighting/config probe is:

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

## Implementation impact

- The current replacement app does not need this startup/config probe for live
  lighting writes.
- Keep these packets documented for future persistent-preset or device-query
  work.
