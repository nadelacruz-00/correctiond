# Contributing

Thanks for helping. correctiond runs inside your input method and sees
everything you type, so changes are judged first on whether they keep typed
text private and never break typing, and only then on features.

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) before a larger change.
Report security problems privately as described in [SECURITY.md](SECURITY.md),
not in an issue.

## Build

You need a stable Rust toolchain, `pkg-config` and the OpenSSL development
headers (SQLCipher is bundled and links the system `libcrypto`). On Debian or
Ubuntu:

```sh
sudo apt-get install -y libssl-dev pkg-config
cargo build --release
```

For the end-to-end and extension tests, also install what CI installs:
`ibus`, `python3-gi`, `gir1.2-ibus-1.0`, `dbus` and `gjs`.

To try a change on your desktop, see the README's Install section:
`scripts/install-dev.sh` once, then `cargo build --release && ibus restart`
after each engine change, and `scripts/install-extension.sh` plus a log out
and back in after each extension change.

## Test

Run these before sending a change. The first group is what CI runs on every
push and pull request (`.github/workflows/ci.yml`).

```sh
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
bash scripts/e2e.sh
python3 -m unittest discover scripts
gjs -m extension/test/lib.test.js
gjs -m extension/test/config.test.js
```

`scripts/e2e.sh` builds the release binary, then runs it against a private
IBus daemon on a private session bus with a temporary `HOME`. It needs no
display.

Not in CI:

| Command | When | Needs |
| --- | --- | --- |
| `bash scripts/headless-smoke.sh` | IBus binding changes | same as e2e; no display |
| `bash scripts/extension-smoke.sh` | extension changes | `gnome-shell` 50 and GJS OSK installed system-wide; runs `gnome-shell --headless` on private buses |
| `gjs -m extension/test/prefs.smoke.js` | Settings window changes | a display and libadwaita 1.7 or later |
| `cargo test -p engine live_keyring -- --ignored` | keyring changes | your real login keyring; creates and deletes one test item |
| `cargo bench -p corelib` | scoring or store performance | |
| `bash scripts/pack-extension.sh` | packaging changes | `zip`, `unzip` |

The headless scripts never touch your desktop session, your input method or
your keyring. Keep it that way: a new test that needs a bus, a daemon or a
`HOME` must create private ones.

Some test data is shared between Rust and JavaScript so the two cannot drift:
`data/status-vectors.tsv` (status codes and menu texts),
`data/learnable-vectors.tsv` and `data/primary-lang-vectors.tsv` (also used by
the importers), and `extension/test/fixtures/config.toml` (what the Settings
window writes and the engine reads). Change both sides together.

## Code style

- **Rust:** `rustfmt` with default settings, and `clippy` with `-D warnings`
  across all targets. No `unwrap` or panics on the typing path: the engine
  must keep typing working on any internal failure (pass the key through).
- **JavaScript (GJS):** ES modules, 4-space indent, single quotes, JSDoc on
  exported functions, matching the existing files. Keep logic that does not
  need GNOME Shell in `extension/lib.js` or `extension/config.js`, with tests
  in `extension/test/` that run under plain `gjs`.
- **Python scripts:** the lexicon tools use the standard library only; test
  them with `unittest` in `scripts/test_*.py`. Run Python with `-I`, as the
  shell scripts do.
- **Shell scripts:** `bash` with `set -euo pipefail`.

## Privacy rules

These are hard rules for code and tests:

- **Never log typed content.** Logs, D-Bus errors, panic messages, traces and
  CLI output use fixed texts and counts only: no characters, words, file
  contents, paths of imported files or key material. The trace API accepts
  only static strings and integers; keep it that way.
- **Never learn more than needed.** Every write to the store goes through
  `is_learnable` and the mode rules in `corelib/src/policy.rs`. Do not add a
  learning path that bypasses them.
- **Synthetic fixtures only.** Tests, fixtures, examples and bug reports must
  use made-up words and text. Never commit real chat exports, typed text,
  learned words, `learn.db` files, keyring contents or traces from a real
  session. The e2e test plants a synthetic marker word and fails if it shows
  up in any log; follow that pattern for new leak checks.
- **No network.** The engine must not open any connection other than the
  IBus daemon and the session bus.
- **No plaintext fallback** for the learning store, and no keyring prompts.

## Commits

Commit messages follow the style already in `git log`:

```text
type(scope): short summary in the imperative, lowercase
```

- Types: `feat`, `fix`, `docs`, `chore`.
- Scopes name the area: `engine`, `corelib`, `ibus`, `control`, `persist`,
  `learning`, `policy`, `extension`, `prefs`, `spec`, `plan`. Leave the scope
  out for changes that span the repo (`docs: ...`, `chore: ...`).
- Examples from the history:
  `feat(control): Shell-only ListWords and ForgetWords`,
  `fix(extension): Cancel, not Forget, is the confirm dialog's default`,
  `fix(corelib): forgetting a word also drops its blacklist rows`.

Keep each commit to one change, with its tests in the same commit.

## License

By contributing you agree that your work is licensed under MIT OR
Apache-2.0, like the rest of the project.
