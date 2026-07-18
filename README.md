# INZONE Buds for Linux

A small native Linux utility that reads battery status from Sony INZONE Buds
through their USB receiver. It reports the left earbud, right earbud, and
charging case separately.

The project is early-stage and currently targets the WF-G700N / YY2977 receiver
with USB ID `054c:0ec2`.

## Why it needs read/write device access

The receiver does not broadcast battery status. The utility sends a
parameterless protocol-level `GET` report and reads the reply. Linux therefore
requires write permission on the HID node even though the operation does not
change settings. No `SET` or firmware commands are implemented.

## Build

Rust 1.85 or newer is required. The CLI has a small Linux system-call
dependency; the optional tray binary also uses `ksni` for the Linux
StatusNotifierItem interface.

```bash
cargo build --release
cargo build --release --features tray
```

## Device permission

For a temporary test, find the receiver's hidraw node and grant your user access:

```bash
sudo setfacl -m "u:$USER:rw-" /dev/hidrawN
```

For regular use, install the included udev rule:

```bash
sudo install -m 0644 contrib/70-sony-inzone-buds.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
```

Then unplug and reconnect the USB receiver. The rule uses systemd-logind's
`uaccess` mechanism and targets only the receiver's vendor HID interface 05.
It grants the active desktop user unrestricted read/write access to that
interface; the utility itself implements only the fixed battery `GET` packet.

## Command-line usage

The receiver is discovered automatically:

```bash
./target/release/inzone-buds
```

Example:

```text
Sony INZONE Buds (/dev/hidraw3)
Left:  54% (discharging)
Right: 56% (discharging)
Case:  90%
```

For scripts and desktop integrations:

```bash
inzone-buds --json
```

Use `--raw` to print the response bytes to stderr while investigating protocol
changes. A manually supplied `--device` is still checked against the expected
Sony USB identity, interface number, report descriptor, and opened character
device before any report is sent. Symlinks and paths outside `/dev/hidrawN` are
rejected.

## KDE tray icon

Start the StatusNotifierItem tray application with:

```bash
./target/release/inzone-buds-tray
```

Clicking the headphone icon opens a menu with the separately reported left,
right, and case percentages. The menu also provides Refresh and Quit actions.
Battery reads run outside the D-Bus menu thread so an unavailable receiver does
not freeze Plasma's tray UI. It queries once at startup, automatically refreshes
once a minute, and also lets you request an immediate refresh. Automatic and
manual requests share one worker and cannot run concurrently.

To install both binaries into Cargo's user binary directory:

```bash
cargo install --locked --path . --features tray
```

To run the tray as a user service after graphical login, install and enable the
included systemd unit:

```bash
install -Dm755 target/release/inzone-buds-tray ~/.local/bin/inzone-buds-tray
install -Dm644 contrib/inzone-buds-tray.service \
  ~/.config/systemd/user/inzone-buds-tray.service
systemctl --user daemon-reload
systemctl --user enable --now inzone-buds-tray.service
```

The service starts with `graphical-session.target`, stops at logout, and
restarts after failures. Choosing Quit from the tray is a clean exit and does
not trigger a restart. The included `inzone-buds-tray.desktop` remains
available as an XDG-autostart fallback on desktops without systemd user units.

## Roadmap

- Connection and firmware-version status
- Packaging for common Linux distributions
- Additional INZONE receiver models, when safely verified

See [docs/protocol.md](docs/protocol.md) for the documented battery protocol.

## Tests

The test suite covers protocol parsing, request and response I/O, discovery and
device verification, CLI output and error branches, tray states and actions,
refresh scheduling, and real CLI/tray process startup. CI requires zero
uncovered code-generated source lines and zero uncovered functions, including
integration-test source. See [CONTRIBUTING.md](CONTRIBUTING.md) for the pinned
coverage command and an explanation of LLVM's duplicate-instantiation summary.

## Disclaimer

This is an independent community project and is not affiliated with or endorsed
by Sony. INZONE and Sony are trademarks of Sony Group Corporation.
