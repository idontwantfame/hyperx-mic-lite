# USBPcap capture: 4-wave

Capture file: `D:\4-wave.pcapng`

This capture was taken with the original HyperX/NGENUITY software while using
the Wave lighting effect and pressing `Save to Microphone`.

## Devices

- Device address `9`: `0951:171d`, USB audio function.
- Device address `10`: `0951:171f`, HID lighting/control function.

Lighting traffic is on device address `10`.

## Native Wave frames

Native Wave uses the same HID `SET_REPORT` transport as solid colors:

```text
21 09 00 03 00 00 40 00
```

Unlike the solid-color capture, native Wave fills most or all of the 64-byte
report with repeated 4-byte LED cells:

```text
81 rr gg bb 81 rr gg bb 81 rr gg bb ...
```

Representative frame:

```text
81 1a 35 00 81 2f 19 00 81 1b 35 00 81 30 19 00
81 1b 35 00 81 30 18 00 81 1b 35 00 81 30 18 00
81 1c 35 00 81 30 17 00 81 1c 35 00 81 30 16 00
81 1d 35 00 81 30 16 00 81 1d 35 00 81 30 15 00
```

That is 16 cells per frame. The current replacement writer only fills the first
two cells, which works for basic lighting but leaves resolution/smoothness on
the table. Native Wave quality should be replicated by extending our frame
builder from 2 cells to 16 cells.

## Save to Microphone

The `Save to Microphone` action appears to produce a short command/ack sequence
around `7.835s` and again around `12.979s`.

Observed write/read/write cluster:

```text
04 02 00 00 00 00 00 00 ...
GET_REPORT -> response: 04 02 00 01 fe 8c 04 ...
04 23 00 00 00 00 00 00 01 00 00 00 ...
GET_REPORT -> response: 04 23 00 01 00 00 00 00 01 ...
08 00 00 00 ... 28 d0 02 aa 55
04 02 00 00 00 00 00 00 ...
GET_REPORT -> response: 04 02 00 01 01 02 00 ...
```

The sequence repeats with a different sentinel-ish payload:

```text
08 00 00 00 ... 28 6c 00 aa 55
```

Interpretation: this is likely a persistent-device-save transaction rather than
normal live lighting. Do not run this on every Apply. If implemented, expose it
as an explicit `Save to Microphone` action with clear UI wording because it may
write to device non-volatile memory.

## Implementation impact

- Extend lighting frames to support 16 `81 rr gg bb` cells.
- Keep live preview/apply separate from persistent `Save to Microphone`.
- Treat the save command sequence as experimental until tested with a minimal
  manual command.
