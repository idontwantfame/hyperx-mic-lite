# USBPcap capture: 7-save-solid

Capture file: `D:\7-save-solid.pcapng`

This capture was taken while selecting Solid lighting and clicking
`Save to Microphone` in the old app.

## Devices

- Device address `9`: `0951:171d`, USB audio function.
- Device address `10`: `0951:171f`, HID lighting/control function.

All lighting/config traffic below is on device address `10`.

## Save sequence

`Save to Microphone` for Solid uses the same command family seen in the Wave
save capture.

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

## Implementation impact

- Add `Save to Microphone` only as an explicit, experimental action.
- Do not call the save sequence automatically after color/effect changes.
