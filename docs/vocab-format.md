# correctiond vocab v1

The standard way to hand correctiond word counts learned elsewhere. Any
converter (a chat-history exporter, a text-corpus counter, the Messenger
importer from [correctiond-import-cookbook](https://github.com/nadelacruz-00/correctiond-import-cookbook)) only has to write this format; `engine import FILE` sends the file
to the running engine, which re-validates everything before anything is
learned. This document is normative.

## File

- Name: `*.vocab.tsv`.
- Encoding: UTF-8 (no byte-order mark), LF line ends.
- The first non-empty line is exactly `#correctiond-vocab v1`.
- Other lines starting with `#` are comments.
- Empty lines are ignored.

## Records

Fields are separated by one TAB.

| First field | Record                                   | Meaning                      |
|-------------|------------------------------------------|------------------------------|
| `w`         | `w<TAB><word><TAB><count>`               | one word with its count      |
| `b`         | `b<TAB><word1><TAB><word2><TAB><count>`  | one adjacent word pair       |
| anything else | ignored                                | forward compatibility        |

- Words are lowercase.
- Counts are positive integers (decimal digits only).

## What readers do

- Lowercase every word.
- Drop any word that fails `is_learnable(word, Mode::Silent)` (3 to 24 ASCII
  letters or apostrophes, starting with a letter, lowercase or Capitalized).
  The check runs on the word as written, so a mixed-case shape such as
  `pAssWord` is dropped, exactly as it is when typed. A word pair is dropped
  when either word fails.
- Clamp counts to 1..=1,000,000.
- Sum duplicate records (the sum is clamped too).
- Reject files over 200,000 records or 16 MiB. Every non-empty line after the
  header that is not a comment counts as a record, including ignored types.
- Skip malformed records and count them: a `w` record without exactly three
  fields, a `b` record without exactly four, or a count that is not a
  positive decimal integer (`0` included). Dropped words count as skipped as
  well.
- A file whose first non-empty line is not the header is rejected whole.

The engine also refuses a file it has already imported (same SHA-256 of the
raw bytes), and refuses every import while the encrypted learning store is
not available (keyring locked or missing), so duplicate tracking can never be
bypassed. Imported counts follow the usual second-sighting rule: a word or
pair whose total count (already learned plus imported) is at least 2 is
written to the encrypted store together with the import record, in one
transaction, before the import reports success. Words and pairs with a total
count of 1 are kept only in memory and are not saved, so importing them
alone does not persist them; they are saved only once they are seen again.

## Example

```
#correctiond-vocab v1
# exported from my notes
w	ako	12
b	ako	naman	3
```

(The separators in the example are TAB characters.)
