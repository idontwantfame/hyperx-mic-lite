# USBPcap capture: 5-vu

Capture file: `D:\5-vu.pcapng`

This capture was taken while changing opacity, direction, and colors for the
native lighting effect. The file was referenced as `5-wu.pcapng`; on disk it is
`5-vu.pcapng`.

## Devices

- Device address `9`: `0951:171d`, USB audio function.
- Device address `10`: `0951:171f`, HID lighting/control function.

Lighting traffic is on device address `10`.

## Native frame behavior

Like the Wave capture, the native app streams HID `SET_REPORT` frames with the
same 64-byte report shape:

```text
81 rr gg bb 81 rr gg bb ...
```

The capture shows repeated native frames after opacity/direction/color changes.
Examples:

```text
81 4d 00 00 81 02 00 00 ...
81 52 30 00 81 03 01 00 ...
81 03 01 00 81 52 30 00 ...
81 01 00 00 81 1c 00 11 ...
```

The reversed examples show that the native app changes direction by reversing
or shifting the cell order, not by sending a separate direction command.

## Implementation impact

The best merge of native behavior and our current app is:

- keep our custom/generated effects as the user-facing model;
- upgrade the packet writer to render up to 16 cells per frame;
- implement direction, opacity, and color changes in our renderer, then stream
  the resulting cells through the same native report format.

This avoids needing to reproduce NGENUITY's internal preset model while still
using the richer native packet layout.
