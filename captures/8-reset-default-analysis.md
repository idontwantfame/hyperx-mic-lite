# USBPcap capture: 8-reset-default

Capture file: `D:\8-reset-default.pcapng`

This capture was taken while using Reset to Default in the old app.

## Devices

- Device address `9`: `0951:171d`, USB audio function.
- Device address `10`: `0951:171f`, HID lighting/control function.

All lighting/config traffic below is on device address `10`.

## Observed command family

The reset-default capture mostly shows the same startup/config command family:

```text
04 56 ... 01 ...
04 57 ... 01 ...
00 00 ... ff 00 ... aa 55
04 02 ...
```

It did not reveal a clearly distinct, safe reset command beyond the generic
startup/config sequence.

## Implementation impact

- Do not implement reset-default from this capture alone.
- Revisit only if there is a smaller isolated capture or a strong reason to
  support reset behavior.
