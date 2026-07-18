# Battery protocol notes

These notes describe the small, read-only subset of the Sony INZONE Buds USB
HID protocol implemented by this project. They were independently derived from
the behavior of INZONE Hub 1.0.19.0 and verified against a retail receiver with
USB ID `054c:0ec2`. No Sony binaries or decompiled source are included.

## HID transport

The receiver exposes the battery transport on USB interface `05`, in a
vendor-defined HID collection with usage page `0xff04`. Battery commands use
report ID `0x02`; input and output reports are 64 bytes. Byte 1 is the number of
meaningful bytes after the two-byte HID header. The implementation verifies all
of these properties before writing to an opened character-device descriptor.

## Battery request

The meaningful portion of a battery request is:

```text
02 0c 01 00 fc 08 96 c3 41 04 01 TT TT CS
```

| Offset | Meaning |
| ---: | --- |
| 0 | HID report ID (`0x02`) |
| 1 | Sony HCI frame length (`0x0c`) |
| 2 | command packet (`0x01`) |
| 3–4 | opcode (`0xfc00`, little-endian) |
| 5 | parameter length (`0x08`) |
| 6–7 | Sony key (`0xc396`, little-endian) |
| 8 | source PC, destination receiver (`0x41`) |
| 9 | battery event (`0x04`) |
| 10 | operation `GET` (`0x01`) |
| 11–12 | transaction ID, little-endian |
| 13 | wrapping sum of bytes 6–12 |

The rest of the 64-byte report is zero padding. This command has no parameters
and uses the protocol's `GET` event type; it does not alter device settings.

## Battery response

A verified response was:

```text
02 12 04 ff 0f 00 96 c3 14 04 10 01 00 00 36 00 38 ff 5a 49
```

The six battery payload bytes begin at report offset 13:

```text
left_state left_percent right_state right_percent case_state case_percent
```

Known states are `0` discharging, `1` charging, `2` error, and `0xff`
unavailable. The response checksum is the wrapping sum of HCI frame bytes 3
through the final payload byte. The final byte of the frame contains that sum.
The current parser requires the verified 18-byte frame shape and retains only
its meaningful bytes for diagnostic output.

The case has no live radio path to the USB receiver while the earbuds are out
of it. A case percentage accompanied by state `0xff` is therefore a retained
snapshot from the last exchange through the earbuds, not a live reading. The
case may charge independently without that value changing.

## Deliberate scope

The wider protocol exposes settings and firmware operations. This project does
not implement them. Any future setting change must be documented, reviewed, and
kept separate from the battery-only path.
