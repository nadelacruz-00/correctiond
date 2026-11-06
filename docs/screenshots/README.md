# Screenshots

Made by `scripts/readme-screenshots.sh` from synthetic data only: a private headless
GNOME Shell 50 (temp HOME, private session and system buses, dark style), the
extension from `extension/`, and `scripts/fake_correctiond.py` in its `readme` mode in
place of the engine. No real desktop, engine, config or learned words are involved.

| Image | Shows |
| --- | --- |
| `quick-settings.png` | Quick Settings with the Autocorrect menu open over a "Text Editor" window: correcting there, 128 corrections, 9 undone, 2,431 words learned, learning on, five invented recent words |
| `settings-general.png` | Settings window, General page: engine status, the OSK layout choice, the settings file |
| `settings-apps.png` | Apps page: the allowlist from a synthetic `config.toml` |
| `settings-languages.png` | Languages page: en, tl and war, with synthetic lexicon headers |
| `settings-learning.png` | Learning page from a synthetic `import-report.json` ([schema](../import-report.md)), cut after "Your words" |
| `settings-words.png` | Words page after access is allowed: 12 invented words with counts and last-used dates, search, sort, the "Show all stored words" switch |
| `osk-ergonomic-split.png` | GJS OSK with the "landscape ergonomic v5" split layout (42% wide, 24% high, 6 px key spacing) over the editor window |

The Settings window is the real `fillWindow` (`extension/prefsWindow.js`), run standalone
by `scripts/readme_screens.js` with stub word calls, so the Words page needs no Shell
prompt. The "Text Editor" window is that script's too.

## Regenerate

```sh
bash scripts/readme-screenshots.sh            # writes docs/screenshots/*.png
bash scripts/readme-screenshots.sh /tmp/shots # or another directory
KEEP=1 bash scripts/readme-screenshots.sh     # keeps the temp dir and its logs
```

It takes about a minute and leaves no processes behind. `osk-ergonomic-split.png` is
made only when gjs-osk is installed in `/usr/share/gnome-shell/extensions` and its
`custom-layout` setting has a layout named "landscape ergonomic v5"; reading that
setting (`gsettings get`, read-only) is the script's only read of your own settings or
data. The dates on the Words page are relative to the day it runs. The PNGs are
compressed with pngquant or optipng when either is installed.
