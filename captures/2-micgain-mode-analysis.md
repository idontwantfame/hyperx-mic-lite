# USBPcap capture: 2-micgain-mode

Capture file: `D:\2-micgain-mode.pcapng`

This capture was taken with the original HyperX/NGENUITY software while changing
mic gain and using the physical mode/polar-pattern dial.

## Devices

- Device address `9`: `0951:171d`, USB audio function.
- Device address `10`: `0951:171f`, HID lighting/control function.

## Mode dial

The physical mode dial reports on device address `10`, endpoint `0x82`, as
8-byte HID interrupt input reports.

Observed sequence:

```text
05 11 02 00 00 00 00 00
05 11 01 00 00 00 00 00
05 11 00 00 00 00 00 00
05 11 01 00 00 00 00 00
05 11 02 00 00 00 00 00
05 11 03 00 00 00 00 00
05 11 02 00 00 00 00 00
05 11 01 00 00 00 00 00
05 11 00 00 00 00 00 00
05 11 01 00 00 00 00 00
05 11 02 00 00 00 00 00
05 11 03 00 00 00 00 00
```

Confirmed mapping remains:

- `05 11 00`: stereo
- `05 11 01`: omni
- `05 11 02`: cardioid
- `05 11 03`: bidirectional

This matches the existing HID monitor/parser.

## Mic gain

No distinct USB HID/vendor control packet was observed for mic gain changes.
After startup, endpoint-zero writes on device address `10` were only repeated
lighting writes:

- `04 f2 ... 01 ...` display header
- `81 32 ff 00 81 32 ff 00 ...` solid color frame

Device address `9` mostly carried continuous USB audio stream packets. There
were no later class/vendor endpoint-zero control writes matching a mic gain
change.

Current interpretation: the original app's mic gain control is probably using
the standard Windows/Core Audio endpoint volume path rather than a separate
HyperX HID packet. That is consistent with the replacement app's current mic
volume slider already working.

## Follow-up

No packet-writer work is needed for mic gain based on this capture. Keep using
Core Audio for mic gain unless a later, more isolated capture proves a native
vendor command exists.
