# correctiond import report

The importer contract has two halves: [vocab-format.md](vocab-format.md) is what an importer
sends to the engine, and this file is the optional report it leaves for the Settings
window's Learning page. Importers live outside this repo; the Messenger one is in
[correctiond-import-cookbook](https://github.com/nadelacruz-00/correctiond-import-cookbook).

The app never writes this file. Without it the Learning page shows "No chat import yet".

## File

- Path: `$XDG_DATA_HOME/correctiond/import-report.json`, normally
  `~/.local/share/correctiond/import-report.json`.
- Mode `0600`, written atomically (temp file in the same directory, then rename); the
  directory is `0700`.
- UTF-8 JSON, one object.
- Counts only. It never holds words, word pairs, names or message text. Failed-check
  names are the only strings besides the timestamp.

## Fields

| Field | Type | Meaning |
| --- | --- | --- |
| `imported_at` | string | UTC ISO 8601, e.g. `2026-11-06T09:30:00Z` |
| `own_messages` | integer | the user's messages read |
| `threads` | integer | conversations read |
| `words` | integer | words imported into the engine |
| `pairs` | integer | word pairs imported |
| `names_skipped` | integer | names left out |
| `gate` | object | `{result, failed_checks}` below |
| `gate.result` | string | `"passed"` or `"failed"` |
| `gate.failed_checks` | array of string | names of failed checks, `[]` when passed (examples: `chat_rate_qwerty`, `fil_rate_osk`, `typo_share`) |
| `lists_installed` | boolean | whether the rebuilt word lists were installed |
| `tl_added` | integer | Tagalog list words added; `0` when not installed |
| `war_added` | integer | Waray list words added; `0` when not installed |
| `replay` | object | `replay[layout][mode]`, see below |
| `typo_share` | object | `{evaluation, production_delta}` |
| `typo_share.evaluation` | number or null | fraction of sampled new list words that are typos |
| `typo_share.production_delta` | number or null | same, for the words the production lists gained |

`tl_added` and `war_added` are the current field names for the two example list languages
(Tagalog and Waray). An importer for another language can use the same two fields until a
generic `lists_added` map exists.

### `replay`

`layout` is `qwerty`, `colemak` or `osk`. `mode` is `lists_only` or `full`. Each mode holds
`baseline` (before the import) and `rebuild` (after), each an object of rates:

| Rate field | Type | In |
| --- | --- | --- |
| `chat_changed_rate` | number 0..1 or null | both modes |
| `fil_changed_rate` | number 0..1 or null | both modes |
| `typo_fix_rate` | number 0..1 or null | both modes |
| `oov_changed_count` | integer or null | both modes |
| `slang_changed_rate` | number 0..1 or null | `full` only |

`null` means there was nothing to measure. Lower changed-rates are better; a higher
`typo_fix_rate` is better.

## How the Learning page reads it

The page only displays; it does not judge. Every field is optional.

- Missing file: one row, "No chat import yet".
- Not readable, not JSON, or JSON that is not an object (array, string, null): one row,
  "The import report could not be read", with the path.
- Valid object: rows for `imported_at`, `own_messages`, `threads`, `words`, `pairs`,
  `names_skipped`, `tl_added`, `war_added` and the gate. A field that is absent or of the
  wrong type shows "n/a" (a date that does not parse also shows "n/a"); it never fails the page.
- `gate` is read as `gate.result`; a plain string `"passed"` or `"failed"` is also
  accepted, and `failed_checks` at the top level is accepted in place of
  `gate.failed_checks`. Anything else shows "n/a". Only string entries of `failed_checks` are shown.
- Replay rows read `replay[layout].full` for the active layout (`correctiond:colemak`
  gives `colemak`, `correctiond:us` gives `qwerty`, anything else `qwerty`): `chat_changed_rate`,
  `typo_fix_rate` and `slang_changed_rate`, each as "rebuild (before: baseline)".
  `lists_only`, `oov_changed_count`, the other layouts' numbers and `typo_share` are not shown.
- `tl_added` and `war_added` show only when `lists_installed` is exactly `true`;
  otherwise they show `0` and a "Word lists were not installed" row appears.
- Stale: when the engine reports fewer learned words than `words` (for example after
  "Forget all learned words"), the page adds "Learned words were forgotten after this
  import". If the engine is not reachable, no stale note is shown.
