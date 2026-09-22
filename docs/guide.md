# User guide

[← README](../README.md)

## Install / update

Once the first release is published, the same command installs or updates the binary:

```sh
curl -fsSL https://raw.githubusercontent.com/smturtle2/adb-input/main/install.sh | sh
```

It detects Linux x86_64 or ARM64, verifies the release checksum, and atomically replaces
`~/.local/bin/adb-input`. It does not require sudo, edit shell profiles, install a service,
or alter firewall rules. Add `~/.local/bin` to your PATH if it is not already there.
A running process keeps its current version until restarted.

Optional: `ADB_INPUT_VERSION=v0.1.0`, `ADB_INPUT_BIN_DIR`, `ADB_INPUT_DATA_DIR`,
`ADB_INPUT_REPOSITORY`, and `ADB_INPUT_RELEASE_BASE`. Apply overrides to the `sh`
side of the pipeline, for example:

```sh
curl -fsSL https://raw.githubusercontent.com/smturtle2/adb-input/main/install.sh |
  ADB_INPUT_VERSION=v0.1.0 sh
```

The installer requires standard Unix utilities, curl, tar, and sha256sum or shasum. Android platform-tools
(`adb`) must be installed separately; `adb-input doctor` checks availability.

## Use

Run `adb-input` without arguments for an interactive terminal UI:

```sh
adb-input
```

The UI uses the alternate screen. Use the arrow keys or `j`/`k` to select an item and
press Enter to confirm; Esc goes back or quits. The home screen shows the selected
connected device and offers Start, Change device, Connect another device, Pair
wireless debugging, Refresh devices, and Exit. Device selection shows each device's
model and connection state. Connection addresses and pairing codes can be edited in
their forms; pairing codes are masked, and errors appear inline while entered values
are preserved. Nothing is captured merely by opening the UI.

During a session, the UI shows the current **DESKTOP** or **PHONE** mode and the
Ctrl+Shift+R shortcut. Ctrl+C or Esc in DESKTOP mode stops the session and returns to
the home screen. Interactive mode requires an ANSI-capable terminal; `NO_COLOR` and
`TERM=dumb` select monochrome output. If the terminal is too small, resize it as
prompted. Terminal state, the cursor, and the alternate screen are restored on normal,
error, and signal exits. Explicit commands remain available for scripts and automation.

Enable Android USB/wireless debugging and authorize this computer. Pairing and
connection ports shown by Android are different and can change after reconnecting.

```sh
adb-input pair PHONE_IP:PAIR_PORT       # code is entered interactively, not stored
adb-input connect PHONE_IP:CONNECT_PORT
adb-input devices
adb-input doctor
adb-input run                          # choose the only online device
adb-input run --device SERIAL          # select one of multiple devices
adb-input run --connect PHONE_IP:PORT   # connect and run in one command
```

USB and TCP connections (including VPN addresses) use the same input protocol.
Start in **DESKTOP** mode. Press and release **Ctrl+Shift+R** to switch to **PHONE**;
press and release it again to return. The chord is reserved and is not typed into
the phone. F12 is a normal forwarded key. Exit with Ctrl+C from desktop mode.
In desktop mode the chord is observed, not reserved in the compositor: the focused
desktop application may also react to Ctrl+Shift+R. This backend does not register
or override desktop-global shortcuts. Phone mode suppresses the chord from Android.

While switching, release all held keyboard keys. This prevents Ctrl/Shift or other
keys from remaining pressed on either side. Each physical or logical input device
is tracked separately; releasing one keyboard does not release a key held on another.

No IP address, device name, `/dev/input/event` number, desktop environment, or
remote-access product is built into the input selection. Devices are discovered by
capabilities and hotplugged devices are discovered during the session. Busy upstream
remapper devices are skipped; their readable logical outputs are handled normally.

## Linux permissions and scope

The CLI uses Linux evdev directly, independently of X11/Wayland. Your account needs
read access to keyboard and pointer input devices. On distributions using an `input`
group this can be granted by the administrator; other distributions use ACLs or udev
rules. **The installer does not change these system permissions.** Use `doctor` to
check what your account can access. Do not run ADB as a different user just to bypass
input permissions, because its pairing keys and server belong to that user.

A remote client must forward the shortcut and input to the Linux host. Input consumed
by a remote client before it reaches Linux cannot be captured here. Key remappings
already applied by the desktop input stack remain in effect.

Supported release hosts: Linux x86_64 and ARM64. Embedded Android agents: arm64-v8a
and x86_64, on devices allowing the ADB shell to access `/dev/uhid`. No phone root is
required on the tested device, but vendor policies can differ. Windows/macOS capture
and 32-bit Android are not implemented.

Supported input: standard keyboard keys and modifiers, up to six non-modifier keys,
Korean language keys, relative pointer motion, five mouse buttons, vertical wheel.
Absolute mouse sources are converted to relative deltas using their reported axis
range (2,000 motion units per full range, adjusted by `--sensitivity`). Touchscreens
and pens are not treated as mice. Media keys, horizontal scrolling, clipboard transfer,
edge switching, and automatic ADB reconnection are outside the current scope.
Android's physical-keyboard layout and IME handle language input.

## Cleanup

On focus-independent switching, device loss, input errors, Ctrl+C or process exit,
the CLI releases local grabs. Android receives explicit key/button releases on a normal
shutdown. If the transport fails, the agent exits after five seconds without a heartbeat;
closing its UHID descriptors removes the virtual devices. Temporary agent files are
removed on normal exit. If a disconnected phone prevents cleanup, the CLI prints the
exact remaining path for removal after reconnecting. No APK or boot service is installed.
