# USBPcap capture: 3-sliders

Capture file: `D:\3-sliders.pcapng`

This capture was taken with the original HyperX/NGENUITY software while changing
the mic monitoring, headphone volume, and mic volume sliders, followed by their
toggle switches.

## Devices

- Device address `9`: `0951:171d`, USB audio function.
- Device address `10`: `0951:171f`, HID lighting/control function.

The slider/toggle traffic is on device address `9`, endpoint `0x00`. It is USB
Audio Class control traffic, not HyperX HID lighting/control traffic.

## Request format

The original app uses class/interface `SET_CUR` requests:

```text
21 01 ...
```

For sliders, the payload length is 2 bytes and the payload is a signed 16-bit
little-endian USB audio volume value. The app sends each volume update twice:
once for channel `1` and once for channel `2`.

For toggles, the payload length is 1 byte:

- `01`: muted/off
- `00`: unmuted/on

## Sliders

Based on the capture order, the observed controls map as follows.

### Mic monitoring

Entity `0x0d`, volume control selector `0x02`:

```text
21 01 01 02 00 0d 02 00  <2-byte value>
21 01 02 02 00 0d 02 00  <2-byte value>
```

Observed signed value range:

```text
-7680 .. 1536
```

### Headphone volume

Entity `0x09`, volume control selector `0x02`:

```text
21 01 01 02 00 09 02 00  <2-byte value>
21 01 02 02 00 09 02 00  <2-byte value>
```

Observed signed value range:

```text
-10240 .. -2304
```

### Mic volume

Entity `0x0a`, volume control selector `0x02`:

```text
21 01 01 02 00 0a 02 00  <2-byte value>
21 01 02 02 00 0a 02 00  <2-byte value>
```

Observed signed value range:

```text
-2048 .. 1792
```

## Toggles

Observed toggle writes:

```text
15.799s  21 01 00 01 00 09 01 00  01
17.287s  21 01 00 01 00 09 01 00  00
23.924s  21 01 00 01 00 09 01 00  01
25.254s  21 01 00 01 00 09 01 00  00
26.541s  21 01 00 01 00 0d 01 00  01
28.566s  21 01 00 01 00 0d 01 00  00
35.227s  21 01 00 01 00 0a 01 00  01
36.097s  21 01 00 01 00 0a 01 00  00
37.404s  21 01 00 01 00 0a 01 00  01
38.717s  21 01 00 01 00 0a 01 00  00
```

The duplicated entity `0x09` and `0x0a` toggle pairs likely come from toggling
the switch more than once or from NGENUITY refreshing/applying state.

## Implementation impact

The replacement app already controls mic volume through Windows/Core Audio. To
replicate NGENUITY more directly, add USB Audio Class control writes for:

- mic monitoring: entity `0x0d`
- headphone volume: entity `0x09`
- mic volume: entity `0x0a`

These should be implemented separately from the HID lighting packet writer.
