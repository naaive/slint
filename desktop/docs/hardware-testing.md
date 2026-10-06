# Hardware Testing

Use this checklist for the first runs of Nimbus on real machines.
Most features have only run against the headless backend and fake D-Bus daemons,
so each step below names what to look for and what to collect when it fails.

Run it once on a laptop (built-in panel, touchpad, battery, lid, Wi-Fi) and once on a desktop (one or more external monitors, wired network).
Record results in a copy of the [result table](#result-table) and attach the output of `data/nimbus-bug-report.sh` to every report.

## Before You Start

1. Install the build prerequisites from the `README.md`, then build and install:

   ```sh
   cargo build --release --manifest-path desktop/Cargo.toml --workspace
   sudo desktop/data/install.sh
   ```

2. Install the runtime pieces each feature relies on.
   Package names are Debian/Ubuntu ones; adapt them for your distribution.
   - Session: `dbus-user-session` (or `dbus-run-session`), `systemd` with logind, `xdg-desktop-portal`, `xdg-desktop-portal-wlr`.
   - Graphics: a Mesa or vendor GL driver with GBM, `libinput`, `libseat` (or `seatd`).
   - Features: `polkitd`, `udisks2`, `network-manager`, `bluez`, `pipewire-pulse` (for `pactl`), `systemd-timedated`, `upower`, `gnome-keyring`.
   - Optional: `xwayland-satellite` and `Xwayland` for X11 apps, `fcitx5` or `ibus` for input methods.
3. Keep a second machine or a phone with SSH access to the test machine.
   A broken compositor can leave the screen black, and logs are easier to fetch remotely.
4. Note your user's groups (`id`): DRM access needs a logind seat or membership in `video`/`seat`, depending on the distribution.

## 1. TTY Launch

1. Switch to a free TTY (Ctrl+Alt+F3) and log in.
2. Run `nimbus-session 2> ~/nimbus-tty.log`.
   It picks the udev backend on a TTY and wraps itself in `dbus-run-session` when there's no session bus.
3. Check:
   - The panel, dock, and wallpaper appear on every connected monitor within a few seconds.
   - The mouse cursor moves and the keyboard layout matches `[input]` in the configuration.
   - Super+Return starts `nimbus-terminal`; `nimbusctl state` in it lists the outputs and windows.
   - Ctrl+Alt+F2 switches to another VT and back without a black screen.
   - Logging out from the power dialog returns to the TTY with exit status 0 (`echo $?`).
4. If the screen stays black, log in over SSH and collect `~/nimbus-tty.log` and `journalctl -b --user`.
   Set `RUST_LOG=debug` for the next try.

## 2. Display Manager

1. Check that `/usr/local/share/wayland-sessions/nimbus.desktop` (or `/usr/share/wayland-sessions/nimbus.desktop` for SDDM) exists.
2. Log out, choose "Nimbus" in GDM, SDDM, or LightDM, and log in.
3. Check:
   - The session starts as in step 1, and `echo $XDG_CURRENT_DESKTOP` in the terminal prints `Nimbus`.
   - `loginctl show-session $XDG_SESSION_ID -p Type` prints `Type=wayland`.
   - Logging out returns to the display manager.
4. Logs go to the display manager's journal: `journalctl -b _UID=$(id -u)` or `~/.local/share/sddm/wayland-session.log` for SDDM.

## 3. Multiple Monitors

1. Connect a second monitor before login, then once more while the session runs.
2. Check:
   - Each monitor gets a panel and its own workspaces, and windows open on the monitor with the pointer.
   - Settings > Displays lists every monitor; changing arrangement, scale, and refresh rate applies at once and survives a restart of the session.
   - Unplugging a monitor moves its windows to a remaining one; plugging it back restores its layout from `[[outputs]]`.
   - Turning a monitor off with Settings and back on works without restarting.
3. Collect `nimbusctl state --json` and `wlr-randr` output if available, before and after each hotplug.

## 4. HiDPI

1. On a panel above about 200 DPI, set scale 2 (and then 1.5) in Settings > Displays.
2. Check:
   - The shell, the Nimbus apps, and a GTK or Qt app are sharp, not blurry or tiny.
   - The cursor has the same size on every monitor when monitors have different scales.
   - Fractional scales don't leave gaps or one-pixel seams between windows and the panel.
3. Take a screenshot with `nimbusctl screenshot ~/hidpi.png` and attach it.

## 5. Suspend, Lid, and Battery

1. Suspend from the power dialog, wait ten seconds, and wake the machine.
2. Close and open the lid (laptop), with and without an external monitor connected.
3. Check:
   - The lock screen is up before the screen turns off, and it's still up after waking; it never shows the desktop first.
   - The password unlocks it, and fingerprint or other PAM modules work if configured.
   - `systemd-inhibit --list` shows a `sleep` delay lock held by `nimbus-shell` before suspend and again after resume.
   - The panel's battery level matches `upower -i $(upower -e | grep BAT)`.
   - The backlight keys change brightness and show the OSD.
4. Collect `journalctl -b -u systemd-logind` and the session log around the suspend.

## 6. Input Devices

1. Keyboard: switch layouts if several are configured, try dead keys and a Compose sequence in the terminal and the launcher.
2. Touchpad: tap to click, two-finger scrolling, natural scrolling from Settings > Input,
   and a three-finger horizontal swipe to change workspace (`[input] workspace_swipe_fingers`).
3. Touchscreen: tapping a window focuses it, and touches land where the finger is, also on a rotated panel.
4. Tablet: the pointer follows the pen; pressure works in an app that supports it, such as Krita.
5. A game that locks the pointer: the pointer stays locked while the window has focus and is released when another window takes focus.
6. Collect `libinput list-devices` and `RUST_LOG=nimbus_compositor::input=debug` session logs for any device that misbehaves.

## 7. XWayland Apps

1. Check `echo $DISPLAY` in the terminal: it prints `:0` (or another number) when `xwayland-satellite` is installed, and nothing otherwise.
2. Start `xeyes`, `xterm`, and an X11-only app such as an older Electron app or Steam.
3. Check:
   - The first X11 app takes a moment to appear while `xwayland-satellite` starts; later ones appear at once.
   - Copy and paste work between an X11 and a Wayland window.
   - Killing `xwayland-satellite` closes X11 apps, and the next X11 app starts it again.
4. Collect the session log lines mentioning `xwayland` and `xwayland-satellite --version`.

## 8. Input Methods With fcitx5

1. Install `fcitx5`, `fcitx5-chinese-addons` (or `fcitx5-mozc`), and set in `~/.config/environment.d/im.conf`:

   ```sh
   XMODIFIERS=@im=fcitx
   ```

   Leave `GTK_IM_MODULE` and `QT_IM_MODULE` unset, so GTK and Qt use text-input-v3.
2. Add `fcitx5 -d` to `autostart` in `config.toml`, log out, and log in.
3. Check:
   - Ctrl+Space switches to the input method in a GTK app (gedit), a Qt app (kate), the launcher search, and the lock screen.
   - The preedit text shows in the field, the candidate popup sits right under the text cursor, and committing inserts the text.
   - The lock screen password field never shows candidates.
   - A right-click menu in a text field opens and takes keys while the input method is on.
4. Chromium and Electron apps need `--enable-wayland-ime --wayland-text-input-version=3`; note whether that works.

## 9. Screen Sharing in a Browser

1. Check `systemctl --user status xdg-desktop-portal xdg-desktop-portal-wlr`.
2. Open <https://mozilla.github.io/webrtc-landing/gum_test.html> in Firefox and Chromium (with `--ozone-platform=wayland`) and share the screen.
3. Check:
   - The portal asks which monitor to share; the preview shows that monitor and moves with it.
   - Sharing stops cleanly when the page closes.
   - While the screen is locked, the shared picture is black.
4. Run `grim ~/grim.png` and `nimbusctl screenshot ~/ctl.png` and attach both.
5. Collect `journalctl --user -u xdg-desktop-portal -u xdg-desktop-portal-wlr`.

## 10. polkit Prompt

1. Run `pkexec true` in the terminal, and change the time zone in Settings > Date & Time.
2. Check:
   - The authentication dialog covers the monitor, shows the action's message, and takes the keyboard.
   - A wrong password shows an error and asks again; the right one succeeds.
   - Cancel and Escape end the request, and `pkexec` reports it was dismissed.
3. If no dialog appears, collect the session log lines mentioning `polkit` and `ls -l /usr/lib/polkit-1/polkit-agent-helper-1`.

## 11. Automount and Removable Media

1. Insert a USB stick with a FAT or ext4 file system while the session is unlocked.
2. Check:
   - A toast "<name> connected" appears, and "Open" opens the stick in Files.
   - Files lists the stick in its sidebar; eject unmounts it and "Safely Remove Drive" powers it off.
   - Inserting a stick while locked doesn't mount it until the screen is unlocked.
3. Collect `udisksctl status` and `udisksctl dump` (the bug report script includes both).

## 12. Network, Bluetooth, Sound, and Keyring

1. Settings > Network: join a WPA2 network with a wrong password, then the right one; forget it.
2. Settings > Bluetooth: pair a phone or headset by confirming the code.
3. Settings > Sound: switch the default output, change the volume, and mute; the panel's volume follows.
4. Run `secret-tool store --label test test test` and `secret-tool lookup test test`; both succeed without a password prompt after login through PAM.

## Logs to Collect

Run the report script and attach its archive:

```sh
desktop/data/nimbus-bug-report.sh            # writes nimbus-report-<date>.tar.gz in the current folder
desktop/data/nimbus-bug-report.sh --stdout   # prints the report instead
```

It collects versions, the session and user journal, `nimbusctl state`, the configuration, GPU and DRM information, and portal status,
and redacts passwords, tokens, and Wi-Fi keys.
Read the report before you share it: it lists your device names, monitor models, and installed packages.

For crashes, also attach the backtrace from `coredumpctl info nimbus-compositor` (or `nimbus-shell`).
For rendering problems, attach a photo of the screen as well as a screenshot, since screenshots come from the compositor's own rendering.

## Result Table

| Step | Laptop | Desktop | Notes |
| --- | --- | --- | --- |
| 1. TTY launch | | | |
| 2. Display manager | | | |
| 3. Multiple monitors | | | |
| 4. HiDPI | | | |
| 5. Suspend, lid, and battery | | | |
| 6. Input devices | | | |
| 7. XWayland apps | | | |
| 8. Input methods | | | |
| 9. Screen sharing | | | |
| 10. polkit prompt | | | |
| 11. Automount | | | |
| 12. Network, Bluetooth, sound, keyring | | | |
