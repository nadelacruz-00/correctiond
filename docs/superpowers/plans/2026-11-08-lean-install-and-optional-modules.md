# Lean Install and Optional Modules Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make correctiond installable on any GNOME 50 distro from prebuilt `.deb`/`.rpm`/tarball plus the EGO extension, with English-only defaults that keep existing tl/war users, GJS OSK support as a live-toggled optional module, first-run guidance, and an `engine doctor` diagnostic.

**Architecture:** PR 1 changes the Rust engine (config handling, a `touch` cargo feature, strict CLI dispatch, `doctor`), the mirrored JS config module (`extension/config.js`), and packaging (`install.sh`, rpm/tarball jobs, a `/usr/bin/correctiond` wrapper). PR 2 changes the GNOME Shell extension (pure helpers in `lib.js`, a new `osk.js` with `OskWatcher`, a config FileMonitor, E1 setup states in Quick Settings and Settings) and all user docs. The engine never acts on `[modules]`; only the extension decides touch.

**Tech Stack:** Rust 2021 (zbus 5, tokio, toml 0.9, serde), GJS ESM for GNOME Shell 50 / libadwaita, bash, Python 3 (test fake engine), GitHub Actions, cargo-deb, cargo-generate-rpm.

**Spec:** `~/.gstack/projects/local-smart-auto-correct-with-learning/lean-install-spec-20261108.md` (sections cited as "spec §N"). Read it with this plan.

## Global Constraints

- GNOME 50 on Wayland only; `shell-version` is not widened.
- No change to encryption, keyring, persistence or scoring code.
- Learned slips stay stored under the layout string `"osktap"` (`corelib/src/db.rs:96,104`).
- No runtime-loaded code on the keystroke path.
- The engine D-Bus API gains no method: `SetOskVisible(b)` stays the only touch method, with no alias and no `UnknownMethod` fallback.
- Defaults are mirrored: any change to `Config::default()` / `Config::default_for()` in `engine/src/config.rs` lands in the same task as the matching change to `defaultConfig()` in `extension/config.js`.
- Default allowlist (Rust and JS): `["firefox", "firefox_firefox", "firefox-esr", "org.mozilla.firefox", "org.gnome.TextEditor"]`.
- Default languages: `en → builtin:en`, plus `tl`/`war` only when `~/.local/share/correctiond/lexicons/tl.txt` / `war.txt` exist.
- `[modules] touch` is written only when the user flips the Settings switch.
- Exit code 64 means a usage error in `engine doctor`, `engine import` and `scripts/install.sh`.
- No log line, doctor output or error text ever contains config file contents, typed words or learned words.
- Installed paths: `/usr/lib/correctiond/engine`, `/usr/share/ibus/component/correctiond.xml`, `/usr/bin/correctiond` (755 shell script).
- All formats link the system `libcrypto.so.3`; no vendored OpenSSL.
- `lib.js` and `config.js` stay pure (no GLib, Gtk or Shell imports).
- Test word in docs and manual tests is `bulid` → `build`, never `teh`.
- Do not stage or commit `extension/extension.js`, `scripts/extension-smoke.sh`, `TODOS.md`, `docs/designs/notes.md` or `snap-check/` until their current uncommitted changes are resolved by the user (see Preconditions).

## Review Focus

1. **config.toml deleted while the Shell runs.** A delete event must re-derive touch as for a missing file (gjs-osk on disk ⇒ on), not keep the last state as for an invalid file. Test: Task 10 `touchAfterRead` vectors and Task 12 smoke case `config-deleted`.
2. **`IBUS_COMPONENT_PATH` with empty entries** (`":/opt/x:"`, trailing colon). Empty entries must be dropped, never read as the current directory. Tests: Task 10 `componentDirs` and Task 6 `component_dirs_skip_empty_entries`.
3. **`$HOME` prefix that is not a path component** (`HOME=/home/u`, path `/home/u2/x`). Doctor must print `/home/u2/x`, not `~2/x`. Test: Task 6 `tilde_replaces_only_whole_home_component`.
4. **`install.sh` run twice** over its own earlier install. The second run must succeed (exit 0) and replace the files, because no package owns them. Test: Task 8 `rerun_is_idempotent`.
5. **Two Settings flips within the 200 ms debounce** (on, then off). The final state is off with no watcher, handler or timer left, and no `SetOskVisible(True)` is sent. Test: Task 12 smoke case `double-flip`.

## Preconditions (before PR 2)

- The checkout has uncommitted, unrelated changes in `extension/extension.js` (`_openSettings`) and `scripts/extension-smoke.sh` (menu Settings item check). Tasks 11, 12, 14 and 17 edit both files. The user must commit or set these aside before PR 2 starts.
- `TODOS.md` and `docs/designs/notes.md` are also uncommitted. Task 19 and Task 20 edit them; same rule.

## Lanes

Tasks in one lane run in order. Different lanes may run in parallel.

| Lane | Shared files | Tasks |
|---|---|---|
| A config | `engine/src/config.rs`, `extension/config.js`, `extension/test/config.test.js`, fixture | 1 → 2 → 3 |
| B engine binary | `engine/Cargo.toml`, `engine/src/main.rs`, `.github/workflows/*.yml` | 4 → 5 → 6 (after Task 2) → 7 (after Task 8) |
| C scripts | `scripts/install.sh`, `scripts/install-dev.sh` | 8 → 9 (needs Task 5's wrapper path only) |
| D extension (PR 2) | `extension/lib.js`, `extension/extension.js`, `extension/prefsWindow.js`, `scripts/extension-smoke.sh` | 10 → 11 → 12 → 13 → 14 → 15 → 16 → 17 |
| E docs (PR 2) | `README.md`, `docs/*` | 18 → 19 (after lane D) |
| F release | — | 20 (after PR 2 merges) |

## File structure

| File | Responsibility | Task |
|---|---|---|
| `engine/src/config.rs` | parse/load config, `default_for`, lenient `Modules`, span errors, `parse_report` | 1, 2, 3 |
| `extension/config.js` | pure parse/serialize, `defaultConfig({legacyLangs})`, `[modules]`, `touchExplicit` | 1, 3 |
| `data/default-langs-vectors.tsv` (new) | legacy-language rule vectors for both suites | 1 |
| `data/correctiond.xml` | engine order (`correctiond:us` first) | 1 |
| `engine/Cargo.toml` | `touch` feature, deb assets (+wrapper), `[package.metadata.generate-rpm]` | 4, 7 |
| `engine/src/state.rs`, `engine/examples/replay.rs` | `cfg!(feature = "touch")` gates | 4 |
| `engine/src/main.rs` | strict first-argument dispatch, exit 64 | 5 |
| `data/correctiond-wrapper.sh` (new) | `/usr/bin/correctiond` script | 5 |
| `engine/src/doctor.rs` (new) | `engine doctor` checks and rendering | 6 |
| `.github/workflows/release.yml`, `ci.yml` | rpm/tarball jobs, gating, attestations, pins, wrapper listing, no-default-features | 4, 7 |
| `scripts/install.sh` (new), `scripts/test_install_sh.sh` (new) | tarball installer and its tests | 8 |
| `scripts/install-dev.sh` | package-owner guard | 9 |
| `extension/lib.js` | pure helpers: `effectiveTouch`, `touchAfterRead`, `gjsOskInstalledIn`, `componentDirs`, `componentInstalledIn`, `setupState`, `SETUP_TEXT` | 10 |
| `extension/osk.js` (new) | `OskWatcher` | 11 |
| `extension/extension.js` | touch gate, FileMonitor, gjs-osk listener, E1 in Quick Settings | 11, 12, 14 |
| `extension/prefsWindow.js` | touch switch, OSK group, `mru-sources`, E1 in Settings, a11y | 13, 15, 16 |
| `extension/wordsPage.js`, `extension/metadata.json` | empty state, EGO description | 16 |
| `scripts/fake_correctiond.py` | `v0.1.0` mode | 17 |
| `scripts/pack-extension.sh`, `scripts/install-extension.sh` | ship `osk.js` | 11 |
| `README.md`, `docs/configuration.md`, `docs/ARCHITECTURE.md`, `CHANGELOG.md` (new), `.github/ISSUE_TEMPLATE/bug.yml` (new), `TODOS.md` | docs | 18, 19 |

---

# PR 1: engine and packaging

### Task 1: Neutral defaults in Rust and JS (lane A)

**Files:**
- Modify: `engine/src/config.rs:33-52` (`Default`), add `Config::default_for`
- Modify: `extension/config.js:18-32` (`defaultConfig`)
- Modify: `data/correctiond.xml` (move the `correctiond:us` `<engine>` block above `correctiond:colemak`)
- Create: `data/default-langs-vectors.tsv`
- Test: `engine/src/config.rs` tests, `extension/test/config.test.js`

**Interfaces:**
- Produces: `pub const DEFAULT_ALLOWLIST: [&str; 5]` in config.rs; `pub fn Config::default_for(home: &Path) -> Config` (lexicon paths unexpanded, `~/…` form, like today); `Config::default()` = English only. JS: `export const DEFAULT_ALLOWLIST` and `export function defaultConfig({legacyLangs = []} = {})`, where `legacyLangs` is an array of codes from `['tl', 'war']`, kept in that order.

- [ ] **Step 1: Create `data/default-langs-vectors.tsv`**

```
# Legacy-language default rule, shared by engine/src/config.rs and
# extension/test/config.test.js. tl_file<TAB>war_file<TAB>expected_codes;
# flags are 0|1 (lexicon file present under ~/.local/share/correctiond/lexicons/).
0	0	en
1	0	en,tl
0	1	en,war
1	1	en,tl,war
```

- [ ] **Step 2: Write the failing Rust tests** in `config.rs`:
  - `default_for_follows_vectors`: for each row, create a temp `HOME`, create `lexicons/tl.txt` / `war.txt` per the flags, assert `Config::default_for(home).languages` codes == the row's codes, and that `tl`'s lexicon is `"~/.local/share/correctiond/lexicons/tl.txt"`.
  - `default_allowlist_is_pinned`: `assert_eq!(Config::default().allowlist, ["firefox","firefox_firefox","firefox-esr","org.mozilla.firefox","org.gnome.TextEditor"])`.
  - Update `defaults_when_file_absent`: languages == `[en builtin:en]` only, allowlist == the 5 entries.
- [ ] **Step 3: Write the failing JS tests** in `config.test.js`:
  - `defaultConfig follows default-langs-vectors.tsv`: read the TSV (same `GLib.file_get_contents` path pattern as `lib.test.js:286`), build `legacyLangs` from the flags, assert `defaultConfig({legacyLangs}).languages.map(l => l.code).join(',')` == the row.
  - `default allowlist matches the engine` : `eq(defaultConfig().allowlist, [the 5 entries])`.
  - Existing tests that assume tl/war defaults: update to the new defaults.
- [ ] **Step 4: Run both and see them fail**

Run: `cargo test -p engine config:: && gjs -m extension/test/config.test.js`
Expected: FAIL on the new tests.

- [ ] **Step 5: Implement** `Config::default_for(home)` (check `home.join(".local/share/correctiond/lexicons/{tl,war}.txt").is_file()`), `Config::default()` = `default_for` with no files, and the JS `defaultConfig({legacyLangs})`. Keep `auto_switch_osk: true` and JS `oskSource: US_ENGINE` unchanged. Reorder `data/correctiond.xml`.
- [ ] **Step 6: Run the tests**

Run: `cargo test -p engine config:: && gjs -m extension/test/config.test.js`
Expected: PASS. `reads_what_the_preferences_window_writes` still passes (fixture has explicit languages).

- [ ] **Step 7: Commit**

```bash
git add engine/src/config.rs extension/config.js extension/test/config.test.js data/correctiond.xml data/default-langs-vectors.tsv
git commit -m "config: English-only defaults that keep existing tl/war lexicons"
```

### Task 2: Engine config handling (lane A)

**Files:**
- Modify: `engine/src/config.rs` (`load_from`, new types)
- Test: `engine/src/config.rs` tests

**Interfaces:**
- Consumes: `Config::default_for(home)` (Task 1).
- Produces:
  - `pub struct Modules { pub touch: Option<bool> }` (`Default`, `PartialEq`), field `pub modules: Modules` on `Config`.
  - `pub struct ConfigError { pub line: usize, pub column: usize, pub key: Option<String> }` (1-based line and column).
  - `pub fn parse_report(text: &str, home: &Path) -> (Result<Config, ConfigError>, Vec<&'static str>)`: no logging; the `Vec` holds warning texts (today only the touch warning). On `Err`, the caller uses `default_for(home)`.
  - `Config::load_from(path, home)` logs and never fails, as today.

- [ ] **Step 1: Write the failing tests**
  - `missing_languages_uses_legacy_rule`: temp `HOME` with `lexicons/tl.txt`; file `allowlist = ["x"]` only; assert codes == `["en","tl"]` and allowlist == `["x"]`.
  - `invalid_file_uses_default_for`: same `HOME`; file `languages = 3\n[[[`; assert codes == `["en","tl"]`.
  - `unreadable_file_uses_default_for`: path is a directory; assert codes == `["en","tl"]`.
  - `touch_true_false_and_absent`: `[modules]\ntouch = true` → `Some(true)`; `false` → `Some(false)`; no table → `None`.
  - `touch_wrong_type_is_ignored`: `allowlist = ["x"]\n[modules]\ntouch = "yes"` → `modules.touch == None`, `allowlist == ["x"]`, warnings == `["correctiond: config.toml modules.touch must be true or false; ignored"]`.
  - `error_position_comes_from_span`: text `"allowlist = [\"a\"]\nbroken = 3\n"` → `Err(ConfigError { line: 2, column: 10, .. })` (column of the span start; adjust the literal if the toml span starts at `=`; assert line exactly).
  - `error_text_has_no_snippet`: render with `fn invalid_line(e: &ConfigError) -> String`; assert it starts with `"correctiond: config.toml is invalid at line 2, column "` and ends with `"; using defaults. Fix or delete ~/.config/correctiond/config.toml"`, and does not contain `"broken = 3"`.
- [ ] **Step 2: Run and see them fail**

Run: `cargo test -p engine config::`
Expected: FAIL (types missing).

- [ ] **Step 3: Implement.** Deserialize into a private `RawConfig` (`#[serde(default)]`, `languages: Option<Vec<LangSpec>>`, `modules: RawModules { touch: Option<toml::Value> }`, other fields as today), then build `Config`: `None` languages ← `default_for(home).languages`; a non-bool `touch` → `None` plus the warning. Line/column come from `toml::de::Error::span()` start offset counted in `text`, never from `Display`. `key`: fill it only if toml 0.9's error exposes a key path; if not, `invalid_line` omits the ` (key K)` part. Every fallback branch in `load_from` uses `default_for(home)`. `load_from` prints `invalid_line(...)` and each warning with `eprintln!`.
- [ ] **Step 4: Run the tests**

Run: `cargo test -p engine config::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add engine/src/config.rs
git commit -m "config: default_for in every fallback, lenient modules.touch, span-based errors"
```

### Task 3: `config.js` `[modules]`, `touchExplicit`, `legacyLangs` (lane A)

**Files:**
- Modify: `extension/config.js` (`parse`, `parseConfig`, `serializeConfig`, header comment)
- Modify: `extension/test/fixtures/config.toml` (add `[modules]\ntouch = true` after `broken`, before `[[languages]]`)
- Test: `extension/test/config.test.js`, `engine/src/config.rs` (`reads_what_the_preferences_window_writes`)

**Interfaces:**
- Consumes: `defaultConfig({legacyLangs})` (Task 1).
- Produces: `parseConfig(text, {legacyLangs = []} = {})`; config objects gain `modules: {touch: boolean|null}` and `touchExplicit: boolean` (true iff the file had `touch`). `serializeConfig(config)` writes `\n[modules]\ntouch = <bool>\n` after the top-level keys and before `[[languages]]` only when `config.touchExplicit`. The `[modules]` table accepts only `touch` (bool); a wrong type or another key is a `ParseError`.

- [ ] **Step 1: Write the failing JS tests**
  - `[modules] touch parses`: `parseConfig('[modules]\ntouch = false\n').config` → `modules.touch === false`, `touchExplicit === true`.
  - `no [modules] means null and not explicit`.
  - `save without the switch keeps the key absent`: `serializeConfig(parseConfig('allowlist = ["x"]\n').config)` does not include `'touch'`.
  - `a flipped switch writes the key`: set `modules.touch = true; touchExplicit = true` → output contains `'[modules]\ntouch = true\n'` before `'[[languages]]'`.
  - `an explicit key survives saves`: parse → serialize → parse keeps `touch === false`.
  - `[modules] above osk_source`: in `'[modules]\ntouch = true\nosk_source = "x"\n'` the `osk_source` line belongs to the `[modules]` table (TOML table semantics), so expect `bad(…, "unknown [modules] setting 'osk_source'")`.
  - `touch inside [[languages]]` → `bad(…, "unknown language setting 'touch'")`.
  - `touch = "yes"` → `bad(…, 'expected true or false')`.
  - `missing languages use legacyLangs`: `parseConfig('allowlist = ["x"]\n', {legacyLangs: ['tl']}).config.languages` codes == `en,tl`.
  - `an invalid file is ok:false`: `parseConfig('[[[', {legacyLangs: ['tl']}).ok === false`.
  - Fixture round trip: the existing fixture test still passes with the new `[modules]` lines.
- [ ] **Step 2: Write the failing Rust tests**
  - In `reads_what_the_preferences_window_writes`, add `assert_eq!(c.modules.touch, Some(true));`.
  - `modules_above_osk_source_matches_js`: text `"[modules]\ntouch = true\nosk_source = \"x\"\n"` → `parse_report` gives `Ok` with `modules.touch == Some(true)` and `osk_source == None` (the key is inside `[modules]`; `RawModules` ignores unknown keys). Record in a comment that JS refuses this file (read-only Settings) while the engine ignores the stray key: both agree it is not a top-level `osk_source`.
- [ ] **Step 3: Run and see them fail**

Run: `gjs -m extension/test/config.test.js && cargo test -p engine config::`
Expected: FAIL.

- [ ] **Step 4: Implement** a single-bracket `[modules]` header in `parse` (replacing the "only [[languages]] tables are supported" error for that one name; any other `[name]` still fails with that message), a `MODULE_KEYS = {touch: 'bool'}` table, `legacyLangs` threaded into `defaultConfig`, and the conditional write in `serializeConfig`. Update the file header comment's accepted-keys list.
- [ ] **Step 5: Run the tests**

Run: `gjs -m extension/test/config.test.js && cargo test -p engine config::`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add extension/config.js extension/test/config.test.js extension/test/fixtures/config.toml engine/src/config.rs
git commit -m "config.js: [modules] touch written only when set; legacyLangs"
```

### Task 4: `touch` cargo feature and no-default-features CI (lane B)

**Files:**
- Modify: `engine/Cargo.toml` (add `[features]`)
- Modify: `engine/src/state.rs:206,255` and the two tests `osk_taps_are_scored_with_osk_geometry`, `backspace_fix_records_slip`
- Modify: `engine/examples/replay.rs:413-417` (`LAYOUTS`) and `:591` (test loop)
- Modify: `.github/workflows/ci.yml` (`test` job)

**Interfaces:**
- Produces: `[features] default = ["touch"]` and `touch = []` in `engine/Cargo.toml`.

- [ ] **Step 1: Add the CI steps first** after the existing `cargo clippy` line:

```yaml
      - run: cargo test --workspace --locked --no-default-features
      - run: cargo clippy --workspace --all-targets --locked --no-default-features -- -D warnings
```

- [ ] **Step 2: Add the feature and a failing test.** In state.rs add `#[cfg(not(feature = "touch"))] fn osk_flag_is_ignored_without_touch()`: same input as `osk_taps_are_scored_with_osk_geometry` (`"hwllp"` + space on Colemak with `osk = true`), assert the result equals the `osk = false` result.

Run: `cargo test -p engine --no-default-features osk_flag_is_ignored_without_touch`
Expected: FAIL (the OSK geometry is still used).

- [ ] **Step 3: Implement.** At both `effective_layout` call sites pass `osk_visible && cfg!(feature = "touch")`. Mark the two state.rs tests `#[cfg(feature = "touch")]`. In replay.rs, make the `"osk"` row and the `"osk"` entry of the test loop conditional on `cfg!(feature = "touch")` (a `fn layouts() -> Vec<(&'static str, LayoutId, bool)>` that filters the row; the test iterates `layouts()` names).
- [ ] **Step 4: Run all four commands**

Run: `cargo test --workspace && cargo test --workspace --no-default-features && cargo clippy --workspace --all-targets -- -D warnings && cargo clippy --workspace --all-targets --no-default-features -- -D warnings`
Expected: all PASS. `cargo run --release --example replay -- <existing args>` default-build output unchanged (compare with a run before the change).

- [ ] **Step 5: Commit**

```bash
git add engine/Cargo.toml engine/src/state.rs engine/examples/replay.rs .github/workflows/ci.yml
git commit -m "engine: touch cargo feature gated at the state.rs call sites"
```

### Task 5: Strict CLI dispatch, exit 64 and the wrapper script (lane B)

**Files:**
- Modify: `engine/src/main.rs:160-165` (`main`), `:101-105` (`import_main` usage)
- Create: `data/correctiond-wrapper.sh` (mode 755)
- Test: unit tests in `engine/src/main.rs`

**Interfaces:**
- Produces:
  - `const CLI_USAGE: &str = "usage: correctiond doctor [--json] | correctiond import FILE";`
  - `const EXIT_USAGE: u8 = 64;`
  - `enum Dispatch<'a> { Engine { register: bool }, Import(&'a [OsString]), Doctor(&'a [OsString]), Help, Usage }` and `fn dispatch(args: &[OsString]) -> Dispatch<'_>`.
  - The wrapper's path `data/correctiond-wrapper.sh`, installed as `/usr/bin/correctiond` (used by Tasks 7, 8).

- [ ] **Step 1: Write the failing tests** (`#[cfg(test)] mod dispatch_tests`):
  - `[]` → `Engine{register:false}`; `["--register"]` → `Engine{register:true}`; `["import","f"]` → `Import(["f"])`; `["doctor","--json"]` → `Doctor(["--json"])`; `["--help"]` → `Help`; `["bogus"]`, `["--register","x"]`, `["-v"]` → `Usage`.
- [ ] **Step 2: Run and see them fail**

Run: `cargo test -p engine dispatch_tests`
Expected: FAIL.

- [ ] **Step 3: Implement** `dispatch` and use it in `main`: `Usage` prints `CLI_USAGE` to stderr and returns `ExitCode::from(EXIT_USAGE)`; `Help` prints `CLI_USAGE` to stdout, exit 0; `Doctor` calls `doctor::run` (Task 6; until then a stub that prints `CLI_USAGE` and exits 64 is fine). `import_main`'s usage error returns `ExitCode::from(EXIT_USAGE)`; its operational failures stay `ExitCode::FAILURE`. Update the module doc comment.
- [ ] **Step 4: Create the wrapper** with exactly the content in spec §10.4.
- [ ] **Step 5: Update the e2e import check** in `scripts/e2e.sh` only if it asserts a usage exit code (it asserts exit 1 for operational errors; those stay 1). Add one case: `"$bin" import` with no file → exit 64 and `usage: engine import FILE`.
- [ ] **Step 6: Run**

Run: `cargo test -p engine && sh -n data/correctiond-wrapper.sh && cargo build --release && bash scripts/e2e.sh`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add engine/src/main.rs data/correctiond-wrapper.sh scripts/e2e.sh
git commit -m "engine: strict first-argument dispatch, exit 64 for usage, correctiond wrapper"
```

### Task 6: `engine doctor` (lane B, after Task 2)

**Files:**
- Create: `engine/src/doctor.rs`
- Modify: `engine/src/main.rs` (`mod doctor;`, `Dispatch::Doctor`)
- Modify: `engine/Cargo.toml` (move `serde_json = "1"` into `[dependencies]` for `--json`)
- Modify: `scripts/e2e.sh` (doctor checks), `tests/e2e/ibus_roundtrip.py` (new case `teach-word`)
- Test: unit tests in `doctor.rs`

**Interfaces:**
- Consumes: `config::parse_report(text, home)`, `ConfigError` (Task 2); `Config::path(home)`.
- Produces:
  - `pub enum Status { Ok, Warn, Fail }` rendering `ok|warn|fail`.
  - `pub struct Check { pub key: &'static str, pub status: Status, pub value: String }`.
  - `pub fn run(args: &[OsString]) -> ExitCode` (args after `doctor`).
  - Pure helpers: `fn exit_code(checks: &[Check]) -> u8` (0/1/2), `fn render_text(checks) -> String`, `fn render_json(checks) -> String`, `fn tilde(path: &Path, home: &Path) -> String`, `fn component_dirs(env: Option<&OsStr>) -> Vec<PathBuf>` (`/usr/share/ibus/component` then non-empty `IBUS_COMPONENT_PATH` entries), `fn data_dirs(xdg_data_home: Option<&OsStr>, xdg_data_dirs: Option<&OsStr>, home: &Path) -> Vec<PathBuf>` (GLib defaults: `~/.local/share`, then `/usr/local/share:/usr/share`), `fn gjs_osk_installed(dirs: &[PathBuf]) -> bool`, `fn check_component(dirs, exe: &Path) -> Check`, `fn check_config(path, home) -> (Check, Option<Config>)`, `fn check_store(path: &Path, home) -> Check`, `fn check_touch(cfg: Option<&Config>, data_dirs) -> (Check, bool /*effective*/)`, `fn check_extension(uuid_info: Option<ExtInfo>, gjs_info: Option<ExtInfo>, effective_touch: bool) -> Check`, with `struct ExtInfo { state: f64, version: String }`.

Check rules (spec §10.3, decisions not fixed by the spec are marked *):
- `component`: ok when some dir has `correctiond.xml` whose `<exec>` canonicalizes to `canonicalize(current_exe())`; warn when found but exec differs; fail when none.
- `ibus`: ok when `NameHasOwner("org.freedesktop.IBus")`, else fail.
- `engine_running`: ok when `org.nadelacruz.Correctiond1` has an owner and `GetStats` (NoAutoStart, 2 s) answers; value = the counts; warn* when no owner.
- `extension`: state `1.0` → ok*; `3.0` (ERROR) or `4.0` (OUT_OF_DATE) → fail; absent (`{}`) → warn, except gjs-osk absent with effective touch false → ok; other states → warn*. Value: `uuid state version` per extension; the `error` field is never read into output.
- `config`: ok/`invalid at line L, column C` (fail*), plus allowlist size, language codes, and per language whether the lexicon file exists (warn* if one is missing).
- `store`: ok with byte size if `learn.db` exists, warn* if absent. Never opened.
- `modules.touch`: ok; value `explicit true|false` or `derived true|false`.
- Usage: unknown doctor flag → stderr `usage: correctiond doctor [--json]`, exit 64.

- [ ] **Step 1: Write the failing unit tests**
  - `exit_code_is_worst_status`: all ok → 0; any warn → 1; any fail → 2.
  - `json_shape`: `render_json` of two checks == `{"checks":[{"key":"a","status":"ok","value":"x"},{"key":"b","status":"warn","value":"y"}]}`.
  - `text_shape`: `"a: ok x\nb: warn y\n"`.
  - `tilde_replaces_only_whole_home_component` (Review Focus 3): `tilde("/home/u/x", "/home/u") == "~/x"`, `tilde("/home/u2/x", "/home/u") == "/home/u2/x"`, `tilde("/home/u", "/home/u") == "~"`.
  - `component_dirs_skip_empty_entries` (Review Focus 2): `component_dirs(Some(":/opt/x:"))` == `["/usr/share/ibus/component", "/opt/x"]`.
  - `component_exec_must_match`: temp dir with a generated XML whose `<exec>` is the temp exe path → ok; another path → warn; no XML → fail.
  - `gjs_osk_dir_rule`: temp data dir with `gnome-shell/extensions/gjsosk@vishram1123.com/` → true; without → false.
  - `touch_explicit_and_derived`: config `Some(false)` → `explicit false`; `None` + gjs-osk dir → `derived true`.
  - `missing_gjs_osk_ok_when_touch_off` and `extension_error_state_fails` (state 3.0 → fail; 4.0 → fail).
  - `config_invalid_reports_position_not_contents`: file `broken = 3` → value contains `invalid at line 1, column`, not `broken`.
  - `unknown_flag_is_usage`: `run(&["--bogus".into()])` returns exit code 64 (test via a `fn parse_args(args) -> Result<bool /*json*/, ()>` helper).
- [ ] **Step 2: Run and see them fail**

Run: `cargo test -p engine doctor::`
Expected: FAIL.

- [ ] **Step 3: Implement** `doctor.rs`. D-Bus: one `zbus::Connection::session()`; `NameHasOwner` through `org.freedesktop.DBus`; `GetStats` and `org.gnome.Shell.Extensions.GetExtensionInfo` to dest `org.gnome.Shell`, path `/org/gnome/Shell`, with `zbus::message::Flags::NoAutoStart` and a 2 s `tokio::time::timeout`; read `state` as `f64`. No keyring, no database open, no prompt, no network. Paths through `tilde`.
- [ ] **Step 4: Add the e2e checks** in `scripts/e2e.sh` (inner part, engine running): create `$tmp/component/correctiond.xml` from `data/correctiond.xml` with `<exec>` set to `$bin`, then

```bash
rc=0; out=$(IBUS_COMPONENT_PATH="$tmp/component" "$bin" doctor) || rc=$?
```

  Assert `ibus: ok` and `engine_running: ok` lines appear and `rc` is 0, 1 or 2. Teach the unique word `zorblaxite` with a new `ibus_roundtrip.py` case `teach-word` (types `zorblaxite ` three times in an `e2e` field with the existing `type()` helper; run as `python3 -I -u "$client" teach-word`, like the `taglish` cases), run `doctor` and `doctor --json`, and fail if either output contains `zorblaxite`. Also assert `"$bin" doctor --bogus` exits 64.
- [ ] **Step 5: Run**

Run: `cargo test -p engine && cargo clippy --workspace --all-targets -- -D warnings && bash scripts/e2e.sh`
Expected: PASS, and `PASS doctor …` lines in the e2e output.

- [ ] **Step 6: Commit**

```bash
git add engine/src/doctor.rs engine/src/main.rs engine/Cargo.toml Cargo.lock scripts/e2e.sh
git commit -m "engine: read-only doctor diagnostics"
```

### Task 7: Packages and release workflow (lane B, after Task 8)

**Files:**
- Modify: `engine/Cargo.toml` (`[package.metadata.deb]` assets, new `[package.metadata.generate-rpm]`)
- Modify: `.github/workflows/release.yml` (its build job also runs on PRs touching the release paths, so the wrapper listing check there is the CI check)

**Interfaces:**
- Consumes: `data/correctiond-wrapper.sh` (Task 5), `scripts/install.sh` (Task 8).
- Produces: release assets `correctiond_<ver>_<deb-arch>.deb`, `correctiond-<ver>-1.<arch>.rpm`, `correctiond-<ver>-<arch>-linux.tar.gz`, each `+ .sha256`; version-free copies `correctiond_amd64.deb`, `correctiond_arm64.deb`, `correctiond.x86_64.rpm`, `correctiond.aarch64.rpm`, `correctiond-x86_64-linux.tar.gz`, `correctiond-aarch64-linux.tar.gz`, each with its own `.sha256` naming the version-free file.

- [ ] **Step 1: deb assets:** add `["../data/correctiond-wrapper.sh", "usr/bin/correctiond", "755"]`.
- [ ] **Step 2: rpm metadata:** `[package.metadata.generate-rpm]` with `name = "correctiond"` and the same assets as the deb list minus `etc/apport/report-ignore/correctiond` (use `source`/`dest`/`mode` keys). Leave requires automatic, so `libcrypto.so.3` comes from auto-req.
- [ ] **Step 3: release.yml triggers and gating:**

```yaml
on:
  push:
    tags: ["v*"]
  workflow_dispatch:
  pull_request:
    paths: [".github/workflows/release.yml", "engine/Cargo.toml", "scripts/install.sh"]
```

  - "Check the tag matches Cargo.toml" gets `if: startsWith(github.ref, 'refs/tags/')`.
  - `release` job: `if: startsWith(github.ref, 'refs/tags/')`, `needs: [build, extension]` (the matrix `build` job now produces all three formats), `permissions: {contents: write, id-token: write, attestations: write}`, and an `actions/attest-build-provenance` step (pinned by SHA like the other actions) over `dist/*` before publishing.
- [ ] **Step 4: build job:** replace `cargo install cargo-deb --locked` with `cargo install cargo-deb --locked --version <pin>` and add `cargo install cargo-generate-rpm --locked --version <pin>` (pick the current release of each and write it in). After `cargo deb`, run `cargo generate-rpm -p engine`, then build the tarball dir `correctiond-<ver>-<arch>-linux/` with `engine`, `correctiond.xml`, `correctiond-wrapper.sh`, `LICENSE-MIT`, `LICENSE-APACHE`, `ATTRIBUTION.md`, `install.sh`, and tar it. Make the version-free copies, then one `.sha256` per file in `dist/`. Assert the wrapper: `dpkg-deb -c dist/*.deb | grep -q './usr/bin/correctiond$'` and `rpm -qlp dist/*.rpm | grep -qx /usr/bin/correctiond` (install `rpm` with apt in the job).
- [ ] **Step 5: Validate locally**

Run: `cargo deb -p engine && dpkg-deb -c target/debian/*.deb | grep usr/bin/correctiond && python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/release.yml'))"`
Expected: the wrapper line prints; YAML loads. (cargo-generate-rpm runs in CI if it is not installed locally.)

- [ ] **Step 6: Commit**

```bash
git add engine/Cargo.toml .github/workflows/release.yml .github/workflows/ci.yml
git commit -m "release: rpm and tarball jobs, dry runs, attestations, version-free assets"
```

- [ ] **Step 7: Dry run after push:** trigger `workflow_dispatch` on the PR branch. Expected: 6 engine artifacts, 6 version-free copies, `.sha256` files and the extension zip uploaded as workflow artifacts; no release created.

### Task 8: `scripts/install.sh` and its tests (lane C)

**Files:**
- Create: `scripts/install.sh` (755)
- Create: `scripts/test_install_sh.sh` (755)
- Modify: `.github/workflows/ci.yml` (`e2e` job: `- run: bash scripts/test_install_sh.sh`)

**Interfaces:**
- Consumes: the wrapper file name `correctiond-wrapper.sh` next to `install.sh` in the tarball (Task 5).
- Produces: `install.sh [--uninstall] [--dry-run] [--force] [--help]`, exit codes 0, 3 (`E_PKG_OWNED`), 4 (`E_COPY`), 5 (`E_OSTREE`), 64 (`E_USAGE`). Reads its payload from its own directory: `engine`, `correctiond.xml`, `correctiond-wrapper.sh`. Test hooks: `INSTALL_SH_OSTREE_FILE` (default `/run/ostree-booted`) and `PATH` stubs for `dpkg`, `rpm`, `pacman`.

Behavior and texts: spec §11.3, verbatim. Order: parse flags → usage check (unknown flag, or `EUID != 0` with empty `DESTDIR`) → ostree check → owner check (skipped with `--force`) → copy or remove. `--dry-run` prints each action as `would install <dest>` / `would remove <dest>` and writes nothing.

- [ ] **Step 1: Write the failing tests** in `scripts/test_install_sh.sh` (plain bash, a `check NAME CMD…` helper, temp payload dir with fake `engine`, the real `data/correctiond.xml` and `data/correctiond-wrapper.sh`, `DESTDIR=$(mktemp -d)`):
  - `fresh_install`: exit 0; the three files exist under `$DESTDIR`; stdout contains ``Now run `ibus restart` as your user (not with sudo)``.
  - `rerun_is_idempotent` (Review Focus 4): second run exit 0, files replaced.
  - `unknown_flag`: `--bogus` → exit 64, stderr has a usage line.
  - `root_required_without_destdir`: run as non-root with `DESTDIR=` → exit 64 (skip when the test runs as root).
  - `ostree`: `INSTALL_SH_OSTREE_FILE` pointing at an existing file → exit 5, stderr contains `[E_OSTREE]`.
  - `owned_by_dpkg` / `owned_by_rpm` / `owned_by_pacman`: a stub on `PATH` that reports an owner for the engine path → exit 3, stderr contains `is owned by package` and `[E_PKG_OWNED]`; `--force` → exit 0.
  - `copy_failure`: make `$DESTDIR/usr/share/ibus/component` read-only → exit 4, stderr contains `[E_COPY]` and `--uninstall`.
  - `uninstall_removes_marked_wrapper`: after install, `--uninstall` → exit 0, engine and component gone, wrapper gone; stdout contains ``Now run `ibus restart` as your user and remove the autocorrect input source``.
  - `uninstall_keeps_foreign_wrapper`: replace the wrapper with a file without `# correctiond wrapper` → `--uninstall` leaves it.
  - `uninstall_owner_check`: owner stub → `--uninstall` exit 3.
  - `dry_run_writes_nothing`: `--dry-run` → exit 0, `$DESTDIR` stays empty, stdout lists 3 `would install` lines.
- [ ] **Step 2: Run and see them fail**

Run: `bash scripts/test_install_sh.sh`
Expected: FAIL (no script).

- [ ] **Step 3: Implement `scripts/install.sh`** (`#!/bin/sh`-compatible bash with `set -eu`): temporaries via `mktemp -p "<destdir>"` then `mv -f`; owner query per present manager: `dpkg -S <path>`, `rpm -qf <path>`, `pacman -Qo <path>`; remove command in the message: `sudo apt remove <pkg>` / `sudo dnf remove <pkg>` / `sudo pacman -R <pkg>`. Never calls `sudo`, never prompts, never runs `ibus`.
- [ ] **Step 4: Run**

Run: `bash scripts/test_install_sh.sh && shellcheck scripts/install.sh`
Expected: every check `ok`; shellcheck clean (if installed).

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh scripts/test_install_sh.sh .github/workflows/ci.yml
git commit -m "scripts: install.sh for the portable tarball"
```

### Task 9: `install-dev.sh` package guard (lane C)

**Files:**
- Modify: `scripts/install-dev.sh` (before the `sudo install` line)

- [ ] **Step 1: Write the guard:** if `dpkg -S /usr/share/ibus/component/correctiond.xml` or `rpm -qf /usr/share/ibus/component/correctiond.xml` succeeds (whichever exists), print to stderr ``component path is owned by package correctiond; run `sudo apt remove correctiond` (or `sudo dnf remove correctiond`) first, or use `IBUS_COMPONENT_PATH` for a dev build`` and `exit 1`.
- [ ] **Step 2: Verify:** add a case to `scripts/test_install_sh.sh` that runs `install-dev.sh` with a `dpkg` stub reporting an owner (and a fake `target/release/engine` via a temp copy of the repo layout or an env override `INSTALL_DEV_BIN`) and expects exit 1 with that text and no `sudo` call (stub `sudo` that fails the test).

Run: `bash scripts/test_install_sh.sh`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add scripts/install-dev.sh scripts/test_install_sh.sh
git commit -m "install-dev.sh: refuse when a package owns the component"
```

---

# PR 2: extension and docs

Start only after the Preconditions are met.

### Task 10: Pure helpers in `lib.js` (lane D)

**Files:**
- Modify: `extension/lib.js`
- Test: `extension/test/lib.test.js`

**Interfaces:**
- Produces (all exported from `lib.js`):
  - `GJS_OSK_UUID = 'gjsosk@vishram1123.com'`
  - `effectiveTouch(parsed: boolean|null, installed: boolean) -> boolean`
  - `touchAfterRead(read, current, installed) -> boolean`, where `read` is `{kind: 'missing'} | {kind: 'invalid'} | {kind: 'ok', touch: boolean|null}` and `current` is `boolean|null` (null at `enable()`): missing → `installed`; ok → `effectiveTouch(read.touch, installed)`; invalid → `current ?? installed`.
  - `gjsOskInstalledIn(dataDirs: string[], isDir: (path: string) => boolean) -> boolean` (checks `<dir>/gnome-shell/extensions/gjsosk@vishram1123.com`).
  - `componentDirs(ibusComponentPath: string|null) -> string[]`: `['/usr/share/ibus/component', ...non-empty entries]`.
  - `componentInstalledIn(dirs: string[], isFile: (path) => boolean) -> boolean` (checks `<dir>/correctiond.xml`).
  - `SETUP_WAIT_MS = 5000`
  - `setupState({componentInstalled, sourceConfigured, sourceCurrent, engineOwned, ownerWaitExpired}) -> null|'no-engine'|'no-source'|'paused'|'starting'|'failed'` (order: spec §8.1).
  - `SETUP_TEXT = {quick: {...}, settings: {...}}`: the per-state strings from spec §8.4, verbatim (keys = state names). The Settings `null` extra line is `correctsInLine(appNames: string[]) -> string` = `` `Corrects in: ${appNames.join(', ')}. Add apps on the Apps page.` ``.
  - `isCorrectiondSource([type, id]) -> boolean` (`type === 'ibus' && id.startsWith('correctiond:')`).

- [ ] **Step 1: Write the failing tests**
  - `effectiveTouch`: `(null,true)→true`, `(null,false)→false`, `(false,true)→false`, `(true,false)→true`.
  - `touchAfterRead` (Review Focus 1): missing+installed → true; missing+not → false; invalid with current true → true; invalid with current null + installed → true; ok `{touch:false}` + installed → false.
  - `gjsOskInstalledIn` with a temp dir from `GLib.dir_make_tmp` and `isDir = p => GLib.file_test(p, GLib.FileTest.IS_DIR)`: present → true, absent → false, `[]` → false.
  - `componentDirs`: `null` → `['/usr/share/ibus/component']`; `':/opt/x:'` → `['/usr/share/ibus/component','/opt/x']` (Review Focus 2).
  - `componentInstalledIn` with a temp dir.
  - `setupState covers every input combination`: loop over all 32 combinations of the 5 booleans; expected from an explicit table in the test written as the spec §8.1 rows (engineOwned → null; else !component → 'no-engine'; …). Assert also the six named examples literally, e.g. `{componentInstalled:true, sourceConfigured:true, sourceCurrent:false, engineOwned:false, ownerWaitExpired:true}` → `'paused'`.
  - `setup texts are the spec strings`: assert each `SETUP_TEXT.quick[state]` and `SETUP_TEXT.settings[state]` equals the spec §8.4 text (copy them as literals).
  - `isCorrectiondSource`.
- [ ] **Step 2: Run and see them fail**

Run: `gjs -m extension/test/lib.test.js`
Expected: FAIL.

- [ ] **Step 3: Implement** the helpers. No GLib import in `lib.js`.
- [ ] **Step 4: Run**

Run: `gjs -m extension/test/lib.test.js`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add extension/lib.js extension/test/lib.test.js
git commit -m "lib.js: pure touch, probe and setup-state helpers"
```

### Task 11: `osk.js` extraction, static import, touch gate at enable (lane D)

**Files:**
- Create: `extension/osk.js`
- Modify: `extension/extension.js` (remove lines for OSK constants 33-38, state 137-140, `enable()` 168-182, `disable()` 238-251, the OSK methods `_oskContainers` … `_restoreSource` 375-498; keep `_onOwner` and `disable()` touch points)
- Modify: `extension/lib.js` (remove `parseOskSource`, `OSK_SOURCE_LINE`, `AUTO_SWITCH_LINE`), `extension/test/lib.test.js` (remove its tests), `extension/test/config.test.js` (remove the `parseOskSource` import and any test using it)
- Modify: `scripts/pack-extension.sh` (`files=` list and `zip` args gain `osk.js`), `scripts/install-extension.sh` (install list gains `osk.js`), `scripts/extension-smoke.sh:463-465` (install list gains `osk.js`)

**Interfaces:**
- Consumes: `Lib.effectiveTouch`, `Lib.touchAfterRead`, `Lib.gjsOskInstalledIn`, `Lib.GJS_OSK_UUID` (Task 10); `parseConfig` from `./config.js` (Task 3).
- Produces:
  - `export class OskWatcher` in `osk.js` with the constructor options and methods in spec §7.4 (`enable()`, `get shown()`, `disable() -> boolean`).
  - In extension.js: `_readConfig() -> {kind:'missing'}|{kind:'invalid'}|{kind:'ok', touch, oskSource}`; `_gjsOskInstalled() -> boolean` (wrapper: `GLib.get_user_data_dir()` + `GLib.get_system_data_dirs()`); `_setTouch(on: boolean)` that constructs or tears down `this._osk` (an `OskWatcher` or null); `_touch: boolean|null`.

- [ ] **Step 1: Write the failing smoke checks** in `scripts/extension-smoke.sh` (scenario list at the end):
  - existing `osk` scenario (gjs-osk enabled, no `config.toml`): all existing OSK checks still pass (touch derives on because gjs-osk is installed in the temp HOME).
  - existing `no-osk` scenario: add `check no-osk "no OSK watcher without gjs-osk" eval_ok no-osk osk-null` where an Eval asserts `t._osk === null`.
  - new scenario `touch-off`: gjs-osk enabled but `config.toml` has `[modules]\ntouch = false` → no `SetOskVisible (True,)` after opening the OSK; `t._osk === null`.
  - new scenario `late-osk` (load order, R3-1): the gjs-osk directory exists but gjs-osk is enabled 3 s after the companion (enable it through Eval/gsettings after start) → `SetOskVisible (True,)` is still seen when the OSK opens.
  - in the `osk` scenario: `disable` then `enable` with no wait, then `disable`: Eval asserts `t._osk === null` and no `SetOskVisible` after the final disable mark except `(False,)` at most once.
  - pack check: `bash scripts/pack-extension.sh "$tmp"` lists `osk.js`; removing `osk.js` from the `files` copy makes the script fail (run it on a temp copy).
- [ ] **Step 2: Run and see them fail**

Run: `bash scripts/extension-smoke.sh`
Expected: FAIL on the new checks.

- [ ] **Step 3: Implement.** Line numbers above are from commit `3ce2972`; the uncommitted `_openSettings` change shifts them by about 24 lines. Move the OSK code into `OskWatcher` unchanged in behavior (latch, settle timer, `SourceRestore`, container signals, its own `extension-state-changed` rescan). `extension.js` imports it statically. In `enable()`: `this._touch = null; this._setTouch(Lib.touchAfterRead(this._readConfig(), null, this._gjsOskInstalled()))`. `_setTouch(true)` wraps `new OskWatcher({...}).enable()` in try/catch: on error, `this._logOnce('osk-construct', \`osk.js could not be loaded (${e.message}); no on-screen keyboard support\`)` and `logError(e)`, and `this._osk = null`. `_setTouch(false)` calls `this._osk.disable()` and sends `SetOskVisible(false)` if it returned true, then restores nothing else (the watcher restores the source). `disable()` calls `_setTouch(false)` before clearing the proxy. `_onOwner` sends `SetOskVisible(this._osk?.shown ?? false)`. `readOskSource` returns `_readConfig().oskSource` (US engine when missing or invalid, as `parseOskSource(null)` did).
- [ ] **Step 4: Run**

Run: `gjs -m extension/test/lib.test.js && gjs -m extension/test/config.test.js && bash scripts/pack-extension.sh /tmp/x && bash scripts/extension-smoke.sh`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add extension/osk.js extension/extension.js extension/lib.js extension/test/lib.test.js extension/test/config.test.js scripts/pack-extension.sh scripts/install-extension.sh scripts/extension-smoke.sh
git commit -m "extension: OskWatcher in osk.js, constructed only when touch is on"
```

### Task 12: Live touch switch and gjs-osk live install (lane D)

**Files:**
- Modify: `extension/extension.js`
- Modify: `scripts/extension-smoke.sh`

**Interfaces:**
- Consumes: `_readConfig`, `_setTouch`, `_gjsOskInstalled` (Task 11); `Lib.touchAfterRead` (Task 10).
- Produces: `CONFIG_DEBOUNCE_MS = 200`; `_configMonitor` (`Gio.FileMonitor`), `_configDebounceId`, `_gjsOskStateId`; `_recomputeTouch()`.

- [ ] **Step 1: Write the failing smoke scenarios**
  - `toggle`: gjs-osk enabled, OSK open; rewrite `config.toml` with `[modules]\ntouch = false` (atomic rename: write `config.toml.tmp`, `mv`) → after 1 s, `SetOskVisible (False,)` seen, `t._osk === null`, the input source is restored; rewrite with `touch = true` → `t._osk !== null`, and opening the OSK sends `SetOskVisible (True,)`. No stack trace.
  - `unrelated-save`: OSK open; rewrite `config.toml` with only a changed `allowlist` → no new `SetOskVisible` call and no input-source change between the marks.
  - `double-flip` (Review Focus 5): write `touch = true` then `touch = false` 50 ms apart → after 1 s, `t._osk === null`, no `SetOskVisible (True,)` between the marks.
  - `config-deleted` (Review Focus 1): start with `touch = false` and gjs-osk installed; delete `config.toml` → `t._osk !== null`.
  - `invalid-toml`: with touch on, write `[[[` → `t._osk` unchanged.
  - `no-config-dir`: start with no `~/.config/correctiond/` → enable succeeds, no stack trace; then create the dir and a file with `touch = false` → either applied live or a `_logOnce` monitor line exists (both acceptable per spec).
  - `live-gjs-osk`: no `[modules]`, gjs-osk not on disk at start → `t._osk === null`; copy the gjs-osk extension into the temp HOME and enable it → `t._osk !== null` without restarting the shell.
- [ ] **Step 2: Run and see them fail**

Run: `bash scripts/extension-smoke.sh`
Expected: FAIL on the new scenarios.

- [ ] **Step 3: Implement.** In `enable()`: `Gio.File.new_for_path(configPath).monitor_file(Gio.FileMonitorFlags.WATCH_MOVES, null)`; on `changed`, restart a 200 ms `GLib.timeout_add` debounce that calls `_recomputeTouch()`. If creating the monitor throws, `_logOnce('config-monitor', \`cannot watch config.toml (${e.message}); touch changes apply at the next enable\`)`. `_recomputeTouch()`: `const next = Lib.touchAfterRead(this._readConfig(), this._touch, this._gjsOskInstalled()); if (next !== this._touch) this._setTouch(next)` (`_setTouch` stores `_touch`). Connect `Main.extensionManager` `extension-state-changed`; when `ext.uuid === Lib.GJS_OSK_UUID` and the last read had no `touch` key, call `_recomputeTouch()`. `disable()` cancels the monitor, removes the debounce source and disconnects the listener.
- [ ] **Step 4: Run**

Run: `bash scripts/extension-smoke.sh`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add extension/extension.js scripts/extension-smoke.sh
git commit -m "extension: touch switch applies live; gjs-osk install re-derives touch"
```

### Task 13: Settings touch switch, OSK group and `mru-sources` (lane D)

**Files:**
- Modify: `extension/prefsWindow.js:79-91` (`currentLayout`), `:154-175` (load with `legacyLangs`), `:393-410` (OSK group), controller
- Test: `extension/test/prefs.smoke.js`

**Interfaces:**
- Consumes: `parseConfig(text, {legacyLangs})`, `touchExplicit` (Task 3); `Lib.effectiveTouch`, `Lib.gjsOskInstalledIn`, `Lib.isCorrectiondSource` (Task 10).
- Produces: `export function legacyLangsOnDisk(home = GLib.get_home_dir()) -> string[]`; `export function gjsOskOnDisk() -> boolean`; `export function currentSource(settings) -> [string, string]|null` (`mru-sources[0]`, else `sources[0]`); controller `setTouch(on: boolean)`; widgets `controller.touchRow` (`Adw.SwitchRow`), `controller.oskGroup`, `controller.oskRow`.

- [ ] **Step 1: Verify on GNOME 50** that `org.gnome.desktop.input-sources` `current` is ignored and `mru-sources[0]` is the active source (switch with Super+Space, read both with `gsettings get`). Record the result in the PR description. If `current` is still honored, stop and ask.
- [ ] **Step 2: Write the failing prefs smoke tests** (existing `fillWindow(window, opts)` harness; add `opts.gjsOskInstalled`, `opts.legacyLangs`):
  - `touch switch shows the derived value`: no file, `gjsOskInstalled: true` → `touchRow.active === true`; `false` → group hidden (`oskGroup.visible === false`).
  - `group visible when touch is explicitly on without gjs-osk`, with description `GJS OSK is not installed`.
  - `flipping the switch writes the key without a restart banner`: toggle → saved text contains `touch = false`; restart banner not pending; no toast.
  - `layout row insensitive while off` with subtitle `Turn on on-screen keyboard support first`.
  - `switch subtitle`: `For the GJS OSK extension. GNOME's built-in keyboard isn't supported yet.`
  - `saving another setting keeps touch absent`: add an app → saved text has no `touch`.
  - `currentSource uses mru-sources first`.
- [ ] **Step 3: Run and see them fail**

Run: `gjs -m extension/test/prefs.smoke.js` (desktop with display and libadwaita ≥ 1.7)
Expected: FAIL.

- [ ] **Step 4: Implement.** Load: `parseConfig(text, {legacyLangs: legacyLangsOnDisk()})`. Switch `Adw.SwitchRow` titled `On-screen keyboard support`; `setTouch(on)` sets `model.modules.touch = on; model.touchExplicit = true; save({needsRestart: false})`. Group visibility and description recomputed in `refreshStatus`. `currentLayout()` uses `currentSource`.
- [ ] **Step 5: Run**

Run: `gjs -m extension/test/prefs.smoke.js`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add extension/prefsWindow.js extension/test/prefs.smoke.js
git commit -m "Settings: on-screen keyboard support switch, written only when flipped"
```

### Task 14: E1 setup guidance in Quick Settings (lane D)

**Files:**
- Modify: `extension/extension.js` (`_refreshMenu` 572-600, menu open handler, `disable()`)
- Modify: `scripts/extension-smoke.sh`, `scripts/smoke-unsafe@correctiond.test/extension.js` only if Eval needs a hook (prefer a plain property)

**Interfaces:**
- Consumes: `Lib.setupState`, `Lib.SETUP_TEXT.quick`, `Lib.componentDirs`, `Lib.componentInstalledIn`, `Lib.isCorrectiondSource`, `Lib.SETUP_WAIT_MS` (Task 10).
- Produces: `_probeSetup() -> state` reading: component via `componentDirs(GLib.getenv('IBUS_COMPONENT_PATH'))` + `GLib.file_test`; `sourceConfigured` from the `org.gnome.desktop.input-sources` `sources` key; `sourceCurrent` from `getInputSourceManager().currentSource` (`type`/`id`); `engineOwned` = `!!this._proxy?.g_name_owner`; `_setupSince` (ms, set when `sourceCurrent` turns true or the owner is lost); `_setupTimerId`. Test hook: `this._componentProbe = null` — when a function, `_probeSetup` uses it instead of the file check (smoke sets it through Eval, never `XDG_DATA_DIRS`).

- [ ] **Step 1: Write the failing smoke scenarios** (fake engine not started, or started late):
  - `e1-no-engine`: probe stubbed false → app line == `Install the correctiond engine package`; toggle subtitle still `Engine not running`.
  - `e1-no-source`: probe true, no correctiond source in `sources` → app line == the `'no-source'` Quick Settings text.
  - `e1-paused`: probe true, `sources` has `('ibus','correctiond:us')` but current is `xkb` → `Paused: another input source is active (Super+Space to switch)`.
  - `e1-starting-failed`: probe true, correctiond source current, fake never starts, menu kept open → `Starting…` first, then within 7 s the `'failed'` Quick Settings text.
  - `e1-live`: fake starts → existing `statusText` line.
  - After `disable()` no `_setupTimerId` remains (Eval).
- [ ] **Step 2: Run and see them fail**

Run: `bash scripts/extension-smoke.sh`
Expected: FAIL.

- [ ] **Step 3: Implement.** In `_refreshMenu()`, call `_probeSetup()`; when non-null, set `_appItem.label.text = Lib.SETUP_TEXT.quick[state]`, hide counts and controls (as `statusText`'s `countsLine = null`, `controls = false`). While the menu is open and the state is `'starting'`, arm one `GLib.timeout_add` of `SETUP_WAIT_MS` that re-runs `_refreshMenu()`; remove it on menu close and in `disable()`.
- [ ] **Step 4: Run**

Run: `bash scripts/extension-smoke.sh && gjs -m extension/test/lib.test.js`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add extension/extension.js scripts/extension-smoke.sh
git commit -m "Quick Settings: first-run setup guidance"
```

### Task 15: E1 setup guidance in Settings (lane D)

**Files:**
- Modify: `extension/prefsWindow.js:355-391` (Status group, `refreshStatus`, Restart row)
- Test: `extension/test/prefs.smoke.js`

**Interfaces:**
- Consumes: `Lib.setupState`, `Lib.SETUP_TEXT.settings`, `Lib.correctsInLine`, `currentSource` (Task 13), probes as in Task 14 (prefs-process wrappers in prefsWindow.js; `opts.componentProbe` test hook).
- Produces: suffix widgets `controller.getEngineLink` (`Gtk.LinkButton`, label `Get the engine`, uri `https://github.com/nadelacruz-00/correctiond#install`), `controller.keyboardButton` (flat `Gtk.Button`, label `Open Keyboard Settings`), `controller.reportLink` (`Gtk.LinkButton`, label `Report a problem`, uri `https://github.com/nadelacruz-00/correctiond/issues/new?template=bug.yml`), all placed before the refresh icon; `restartedWhileFailed: boolean`.

- [ ] **Step 1: Verify** the desktop ID `gnome-keyboard-panel.desktop` exists on GNOME 50 (`ls /usr/share/applications/gnome-keyboard-panel.desktop`). Record per distro during Task 20.
- [ ] **Step 2: Write the failing prefs smoke tests:**
  - `'no-engine'`: GetStats fails, probe false → status subtitle == Settings `'no-engine'` text, `getEngineLink.visible`, Restart row `sensitive === false`.
  - `'no-source'`: subtitle == the Settings `'no-source'` text; `keyboardButton.visible`; Restart row subtitle == `Makes a newly installed engine show up in the input source list`.
  - `keyboard button failure`: `opts.launchKeyboardPanel = () => false` → toast `Couldn't open Keyboard Settings. Open Settings › Keyboard › Input Sources yourself.`
  - `keyboard button hidden without app info`: `opts.keyboardAppInfo = null` → hidden.
  - `'starting' then 'failed'`: source current, GetStats keeps failing → `Starting…`, then after the 5 s re-probe the Settings `'failed'` text; `reportLink.visible === false`; press Restart, still failing → `reportLink.visible === true`.
  - `live status adds Corrects in`: GetStats ok, allowlist `['firefox']` → subtitle ends with `Corrects in: Firefox. Add apps on the Apps page.` (app names from the existing Apps page lookup).
  - `re-probe on mru-sources change`: emitting `changed::mru-sources` re-runs the probe.
- [ ] **Step 3: Run and see them fail**

Run: `gjs -m extension/test/prefs.smoke.js`
Expected: FAIL.

- [ ] **Step 4: Implement.** `refreshStatus` computes the state after the GetStats reply (owned = success). Connect `changed::sources` and `changed::mru-sources` on the window's own `Gio.Settings`. After a not-owned result with the source current, schedule one `GLib.timeout_add(SETUP_WAIT_MS)` re-probe (removed when the window closes). The keyboard button uses `Gio.DesktopAppInfo.new('gnome-keyboard-panel.desktop')`, hidden when null; `launch([], null)` in try/catch, toast and `log(e.message)` on throw or false.
- [ ] **Step 5: Run**

Run: `gjs -m extension/test/prefs.smoke.js`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add extension/prefsWindow.js extension/test/prefs.smoke.js
git commit -m "Settings: first-run setup guidance and state-aware Restart row"
```

### Task 16: Words empty state, EGO metadata, accessibility (lane D)

**Files:**
- Modify: `extension/wordsPage.js` (empty state), `extension/metadata.json` (`url`, `description`), `extension/prefsWindow.js` (refresh icon)
- Test: `extension/test/prefs.smoke.js`

- [ ] **Step 1: Write the failing prefs smoke tests:**
  - `words empty state`: ListWords returns `[]` → the page shows `Nothing learned yet. correctiond learns as you type and fix words. To start with your own vocabulary, see correctiond-import-cookbook.`
  - `refresh icon is labeled`: `tooltip_text === 'Refresh status'` and its accessible label is `Refresh status`.
  - `every new button has a label`: `getEngineLink`, `keyboardButton`, `reportLink` have non-empty `label`.
- [ ] **Step 2: Run, see them fail, implement, run again**

Run: `gjs -m extension/test/prefs.smoke.js`
Expected: FAIL, then PASS after the changes.

- [ ] **Step 3: metadata.json:** `"url": "https://github.com/nadelacruz-00/correctiond"`; `description` = sentence 1 `Companion for the correctiond autocorrect engine, which you install separately: https://github.com/nadelacruz-00/correctiond/releases`, sentence 2 about the Quick Settings menu and Settings, last sentence: on-screen keyboard (GJS OSK) support is optional. Check: `python3 -c "import json;d=json.load(open('extension/metadata.json'));assert d['description'].startswith('Companion for the correctiond autocorrect engine, which you install separately')"`.
- [ ] **Step 4: Commit**

```bash
git add extension/wordsPage.js extension/metadata.json extension/prefsWindow.js extension/test/prefs.smoke.js
git commit -m "extension: Words empty state, EGO description, labeled refresh button"
```

### Task 17: v0.1.0 method-set fake engine (lane D)

**Files:**
- Modify: `scripts/fake_correctiond.py` (new mode `v0.1.0`)
- Modify: `scripts/extension-smoke.sh` (one scenario)
- Modify: `docs/ARCHITECTURE.md` (§ D-Bus interfaces: compatibility policy)

**Interfaces:**
- Produces: mode `v0.1.0` whose XML is exactly the method set of the `v0.1.0` tag (today: the full `XML` with `NEW_METHODS`, frozen as a separate constant `V0_1_0_XML` so later additions to the fake do not change it).

- [ ] **Step 1: Write the failing smoke scenario** `v010`: the fake in `v0.1.0` mode → menu checks pass, no stack trace, no `UnknownMethod` journal line from the extension.
- [ ] **Step 2: Run, see it fail (unknown mode), implement the mode, run again**

Run: `bash scripts/extension-smoke.sh`
Expected: FAIL, then PASS.

- [ ] **Step 3: Write the policy** into `docs/ARCHITECTURE.md`: the engine never removes or renames a method on `org.nadelacruz.Correctiond1` (deprecated names stay as aliases); every method added after v0.1.0 gets an `UnknownMethod` fallback in the extension like `GetStatus`; enforced by the `v0.1.0` fake mode in extension-smoke.
- [ ] **Step 4: Commit**

```bash
git add scripts/fake_correctiond.py scripts/extension-smoke.sh docs/ARCHITECTURE.md
git commit -m "test: v0.1.0 method-set fake engine; document the compatibility policy"
```

### Task 18: README (lane E)

**Files:**
- Modify: `README.md` (`## Install` 119-222, `## How it works` mermaid, `## Troubleshooting` 336-360)

Content: spec §13.1, every bullet. Exact items the reviewer checks:
- [ ] **Step 1: Install section:** keep the `## Install` heading; open with three lines (no network code; encrypted store; keystrokes never logged) linking `SECURITY.md`; sections with anchors `#install-ubuntu`, `#install-fedora`, `#install-other`; one copy-paste block per distro:
  - Ubuntu: ``curl -LO https://github.com/nadelacruz-00/correctiond/releases/latest/download/correctiond_$(dpkg --print-architecture).deb`` then `sudo apt install ./correctiond_*.deb`.
  - Fedora: `curl -LO …/correctiond.$(uname -m).rpm` then `sudo dnf install ./correctiond.$(uname -m).rpm`; ostree: `rpm-ostree install ./correctiond.$(uname -m).rpm` (the README uses the downloaded name; `install.sh`'s `E_OSTREE` text stays as the spec fixes it).
  - Other: `curl -LO …/correctiond-$(uname -m)-linux.tar.gz -LO …/correctiond-$(uname -m)-linux.tar.gz.sha256`, `sha256sum -c correctiond-$(uname -m)-linux.tar.gz.sha256`, extract, `sudo ./install.sh`; optional `gh attestation verify <file> -R nadelacruz-00/correctiond`; verification-failure guidance; "requires OpenSSL 3 (present on every GNOME 50 distro)"; the `sudo DESTDIR=…` note.
  - Then "run `ibus restart` as your user (not with sudo)"; the Add Input Source click path (placeholder `TODO-VM` marked for Task 20 to fill with the verified string); the EGO button and the `gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell --method org.gnome.Shell.Extensions.InstallRemoteExtension correctiond-companion@nadelacruz-00.github.io` line; the release-zip `gnome-extensions install` + relogin fallback until EGO approval.
- [ ] **Step 2: Check it works:** type `bulid ` in Firefox → `build `; Backspace right after undoes it. No "forget" step.
- [ ] **Step 3:** First-week line with the correctiond-import-cookbook link; "Upgrading from a source install" (spec §12); "Build from source" (move the current steps 1-7 here unchanged); "Uninstall" per format (spec §13.1).
- [ ] **Step 4: Troubleshooting:** step 1 "run `correctiond doctor`"; one `###` entry (and anchor) per E1 state (`no-engine`, `no-source`, `paused`, `starting`, `failed`) and per error code (`E_USAGE`, `E_PKG_OWNED`, `E_COPY`, `E_OSTREE`); "package conflict on upgrade from source"; "downgraded extension shows read-only Settings: delete the `[modules]` block"; "after updating the extension from EGO, log out once so the Shell loads the new version"; keep the existing items that still apply and the link to `docs/troubleshooting.md`. "Questions and bugs: GitHub Issues".
- [ ] **Step 5:** Mermaid label `OSK` → `on-screen keyboard`.
- [ ] **Step 6: Verify:** `grep -nw teh README.md` prints nothing; every in-page link `](#…)` has a matching heading (check with a short script that lists `#anchors` and GitHub-slugified headings); `curl` asset names match Task 7's version-free names.
- [ ] **Step 7: Commit**

```bash
git add README.md
git commit -m "README: package install per distro, check it works, uninstall, troubleshooting"
```

### Task 19: Other docs (lane E)

**Files:**
- Modify: `docs/configuration.md` (new `### [modules]` under `## Keys`), `TODOS.md` (Autocomplete entry wording only)
- Create: `CHANGELOG.md`, `.github/ISSUE_TEMPLATE/bug.yml`

- [ ] **Step 1: configuration.md `[modules]`:** the `touch` key; derivation with the directories (`$XDG_DATA_HOME` or `~/.local/share`, and each `XDG_DATA_DIRS` entry, `…/gnome-shell/extensions/gjsosk@vishram1123.com`); `osk_source`/`auto_switch_osk` apply only while touch is on; written only when the switch is flipped; engine ignores and logs a wrong type, Settings goes read-only; the verified `IBUS_COMPONENT_PATH` behavior (filled in Task 20). Update the "Full example" and the defaults text (English only, legacy tl/war rule, 5-entry allowlist).
- [ ] **Step 2: CHANGELOG.md:** `## v0.1.0` with "Upgrading" (source → package; the touch switch release note: `If you use the GJS OSK extension, on-screen keyboard support is now a switch in Settings, General.`; explicit `allowlist` configs don't gain Text Editor).
- [ ] **Step 3: bug.yml:** fields distro, GNOME version, install method, `correctiond doctor` output, `journalctl --user -b | grep correctiond` lines.
- [ ] **Step 4: TODOS.md:** in the Autocomplete entry, replace "(deferred from this round if the gate accepts that)" with "(deferred from this round by the gate)". Change nothing else (the list already matches spec §20).
- [ ] **Step 5: Verify:** `python3 -c "import yaml;yaml.safe_load(open('.github/ISSUE_TEMPLATE/bug.yml'))"`.
- [ ] **Step 6: Commit**

```bash
git add docs/configuration.md CHANGELOG.md .github/ISSUE_TEMPLATE/bug.yml TODOS.md
git commit -m "docs: [modules] reference, CHANGELOG, bug template"
```

### Task 20: Release verification (lane F, manual, after PR 2 merges)

**Files:** `docs/designs/notes.md` (results), `README.md` and `docs/configuration.md` (verified strings, in a small follow-up PR before tagging)

- [ ] **Step 1:** Run `workflow_dispatch` on main; download the artifacts.
- [ ] **Step 2:** On clean VMs (Ubuntu 26.04 with snap Firefox, Fedora, Arch; confirm GNOME 50 on each first): install the engine artifact and the extension zip with `gnome-extensions install`; follow the README only; clock README Install heading → first `bulid `→`build ` in the default Firefox. Target under 5 minutes; record steps and minutes in `docs/designs/notes.md`.
- [ ] **Step 3:** Per app (default Firefox, then GNOME Text Editor), in this order: `bulid ` → `build `; Backspace right after gives `bulid `; forget `bulid` (Quick Settings recent words or Settings › Words). In Text Editor, also trace content hints to confirm `NO_SPELLCHECK` handling. If Text Editor fails anywhere, remove `org.gnome.TextEditor` from both default lists (one task, Rust and JS together, with the parity tests) before tagging, and record it.
- [ ] **Step 4:** Extension-first order on one VM: E1 shows `'no-engine'` → `'no-source'` → `'paused'` or `'starting'` → live.
- [ ] **Step 5:** Verify and record: dpkg/rpm overwrite of the unowned component (if either refuses, add the one `sudo rm /usr/share/ibus/component/correctiond.xml` step to the CHANGELOG "Upgrading" section and the release notes); Add Input Source entry text and click path (fill the README `TODO-VM`, and match the E1 "English (US, autocorrect)" copy in `lib.js` `SETUP_TEXT` if it differs); `gnome-keyboard-panel.desktop`; `IBUS_COMPONENT_PATH` replaces or adds (fill docs/configuration.md); the `gdbus` EGO install command; relogin after EGO install; snap Firefox app id `firefox_firefox`; `rpm -qpR` shows `libcrypto.so.3`.
- [ ] **Step 6:** Upgrade from source on the author's machine: Words page count unchanged, tl/war kept, touch on with gjs-osk.
- [ ] **Step 7:** Tag `v0.1.0`; post-tag check installs the published `.deb`, `.rpm` and tarball (after `sha256sum -c` and `sudo ./install.sh`); submit the zip to EGO.
