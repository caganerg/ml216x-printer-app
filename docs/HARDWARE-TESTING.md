# Hardware testing and reconnect investigation

## Maintainer report, 2026-09-06

After installing the Debian package on their own system, the maintainer
reported that printing through the IPP queue works and that sharing that
queue through CUPS works for other devices on the local network.
This is user-reported hardware evidence, not a test run on the development
host. The reported environment is Debian testing with GNOME; the USB queue
identifies the device as Samsung ML-2160 Series. The installed package version,
media, resolution and measured margins have not yet been recorded.
G-1 and Q-13 therefore remain open.

## Open: duplicate desktop queue on USB reconnect

The maintainer clarified that no package reinstall, PAPPL printer recreation
or IPP queue recreation is needed. After USB reconnect or a printer power
cycle, a new ML2160 printer appears in GNOME alongside the existing IPP queue,
and a driver-required notification is displayed. Deleting the newly created
printer restores use of the existing IPP queue and printing works again.

This supersedes the initial interpretation of a PAPPL reconnect failure.
There is no reported evidence that the IPP registration is lost or that the
PAPPL service must restart. The remaining issue is desktop integration:
duplicate automatic printer creation and selection of the wrong queue.
The exact component creating the queue and whether the system default,
user default or application's last-used printer changes remain unconfirmed.

Next evidence: record `lpstat -t` before and after reconnect, including both
queue names and device URIs, plus the printer model, distribution/version and
USB IDs from `lsusb`. Determine which auto-configuration component creates
the extra queue before implementing a narrowly scoped integration fix.
Do not globally disable discovery for unrelated printers.

A useful interim check is to select the existing IPP queue explicitly as the
default in GNOME and as the destination in the print dialog. This does not
prevent the duplicate queue or its driver-search notification from appearing.
The acceptance test is reconnect / power cycle with the IPP queue retained,
no misleading duplicate queue, and successful printing without manual deletion.

## Queue evidence supplied by the maintainer

`lpstat -t` reports:

- System default destination: `ML2160`.
- `ML2160`: `ipp://127.0.0.1:8631/ipp/print/ML2160`.
- `ML-2160-Series`: `usb://Samsung/ML-2160%20Series?serial=…`.
- Both queues are idle, enabled and accepting jobs.

The serial number is omitted here because device identity can be established
without publishing it. The output rules out a changed system default or a
stopped queue at capture time. User/application printer selection still needs
to be distinguished from the system default.

On the development host, Debian's `system-config-printer-udev` 1.5.18-4
installs `70-printers.rules`. Its USB-device add rule requests
`configure-printer@usb-$env{BUSNUM}-$env{DEVNUM}.service` via `SYSTEMD_WANTS`.
This provides a concrete likely source for the duplicate raw USB queue, but
the corresponding rule/version on the maintainer's testing system needs
confirmation. Collect `lsusb` and `/usr/lib/udev/rules.d/70-printers.rules`
from that system before writing a device-scoped exception. Preserve other
printers' automatic setup and any unrelated systemd device dependencies.

## Device-scoped fix prepared in alpha-3

The maintainer supplied USB ID `04e8:330f` and confirmed the same
`70-printers.rules` add/remove rules on Debian testing. The package now ships
`71-ml216x-printer-app.rules`. It clears `SYSTEMD_WANTS` only for that USB
device when its value is a single `configure-printer@usb-*.service` request.
Values containing whitespace (and therefore potentially other dependencies)
are left untouched. udev does not support `-=` on ENV keys, so removing a list
item using that operator is not valid.

The rule does not hide USB interfaces, change permissions, register printers,
or delete existing queues. Installation reloads rules for subsequent events;
the administrator removes the existing raw USB queue once. The distribution's
remove-event rule remains unchanged. USB IDs for other models are not assumed.

Pending acceptance on the maintainer's hardware: upgrade to alpha-3, remove
only `ML-2160-Series`, reconnect and power-cycle, then verify that only the
working IPP queue remains and local/network printing still works. Do not mark
this passed from a syntax check or package build.
