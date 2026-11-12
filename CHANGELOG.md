# Changelog

## v0.1.0

First packaged release: `.deb`, `.rpm` and a tarball with `install.sh`, plus
the companion extension.

### Upgrading

- **From a source install to the package.** Install the package. It replaces
  the component file in place. If your package manager refuses because
  `/usr/share/ibus/component/correctiond.xml` is not owned by a package, run
  `sudo rm /usr/share/ibus/component/correctiond.xml` first and install again.
  Your settings, lexicons, `learn.db` and keyring item stay as they are. See
  [Upgrading from a source install](README.md#upgrading-from-a-source-install).
- **On-screen keyboard support is now a switch.** If you use the GJS OSK
  extension, on-screen keyboard support is now a switch in Settings, General.
  With no `[modules]` block in `config.toml`, the switch is on when GJS OSK is
  installed, so your current behavior does not change. See
  [`[modules]`](docs/configuration.md#modules).
- **Default allowlist.** The default allowlist now includes
  `org.gnome.TextEditor`. A config with an explicit `allowlist` does not gain
  Text Editor. Add `org.gnome.TextEditor` to your list if you want it.
