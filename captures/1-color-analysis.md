# USBPcap capture: 1-color

Capture file: `D:\1-color.pcapng`

This capture was taken with the original HyperX/NGENUITY software while changing
solid lighting colors. The target USB function for lighting/control traffic is
device address `10`, USB ID `0951:171f`.

## Relevant writes

Filter used for the interesting native writes:

```text
usb.device_address == 10 && usb.endpoint_address == 0x00 && usb.data_len == 72
```

These are USB HID control transfers. A representative packet has setup bytes:

```text
21 09 00 03 00 00 40 00
```

Decoded:

- `bmRequestType = 0x21`
- `bRequest = 0x09` (`SET_REPORT`)
- `wValue = 0x0300`
- `wIndex = 0`
- `wLength = 64`

The 64-byte payload starts after the setup bytes.

## Confirmed payloads

The native app repeatedly sends the same display header that this replacement
already uses:

```text
04 f2 00 00 00 00 00 00 01 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
```

Native solid red frame:

```text
81 ff 00 00 81 ff 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
```

Native green-ish frame observed in this capture:

```text
81 32 ff 00 81 32 ff 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
```

The frame shape is:

```text
81 rr gg bb 81 rr gg bb 00 ...
```

That matches the current packet writer's frame builder for the two lighting
zones.

## High-count payloads

- 628 writes: `04 f2 ... 01 ...` display header
- 147 writes: solid red `ff0000` for both zones
- 104 writes: solid `32ff00` for both zones
- 47 writes: solid `f20000` for both zones

## Other startup/config packets

The capture also includes a short startup/config sequence before the repeated
display header and frame writes:

```text
04 58 ... 01 ...
04 56 ... 01 ...
04 57 ... 01 ...
00 00 ... ff 00 ... aa 55
04 02 ...
```

These are not needed for the current solid-color writer because the native
frame/header sequence already matches what works on the device. Keep them as
protocol notes for later if persistent hardware presets or native effect
selection need to be replicated more exactly.

## Next captures

Use separate, short captures for unknown behavior:

- Audio controls: mic monitoring slider, headphone slider, and any native app
  gain/dial state.
- Native VU Meter: select VU Meter, tap/speak into the mic, then stop capture.
- Native Wave: change speed/opacity/colors and stop capture.
