import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "clean-lexicon.py")


class CleanLexiconTest(unittest.TestCase):
    def run_clean(self, lang, lexicon, english):
        with tempfile.TemporaryDirectory() as d:
            lx, en = os.path.join(d, "lx.txt"), os.path.join(d, "en.txt")
            with open(lx, "w", encoding="utf-8") as f:
                f.write(lexicon)
            with open(en, "w", encoding="utf-8") as f:
                f.write(english)
            subprocess.run(
                [sys.executable, SCRIPT, "--lang", lang, "--english", en, "--in", lx, "--out", lx],
                check=True, capture_output=True,
            )
            with open(lx, encoding="utf-8") as f:
                return [l for l in f.read().splitlines() if not l.startswith("#")]

    def clean_mode(self, *extra):
        with tempfile.TemporaryDirectory() as d:
            lx, en = os.path.join(d, "lx.txt"), os.path.join(d, "en.txt")
            with open(lx, "w", encoding="utf-8") as f:
                f.write("ang\t5000\n")
            with open(en, "w", encoding="utf-8") as f:
                f.write("the\t5\n")
            r = subprocess.run(
                [sys.executable, SCRIPT, "--lang", "tl", "--english", en, "--in", lx,
                 "--out", lx, *extra], capture_output=True, text=True)
            return r, os.stat(lx).st_mode & 0o777

    def test_default_mode_is_0644(self):
        r, mode = self.clean_mode()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(mode, 0o644)

    def test_mode_flag_sets_output_mode(self):
        r, mode = self.clean_mode("--mode", "0600")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(mode, 0o600)

    def test_invalid_mode_is_rejected(self):
        for bad in ["abc", "0999", "07777", ""]:
            r, _ = self.clean_mode("--mode", bad)
            self.assertEqual(r.returncode, 2, bad)
            self.assertIn("--mode", r.stderr)

    def test_drops_english_names_and_taxa_keeps_core_words(self):
        lexicon = (
            "# lexicon lang=tl entries=7\n"
            "ang\t5000\nat\t3000\nthe\t200\njohn\t100\nkumusta\t50\npolice\t40\nhominidae\t9\n"
        )
        # "at" is as common in English as in this toy Tagalog list, so only
        # the keep list saves it; "the", "john", "police" are dropped.
        english = "the\t10000\nat\t3000\njohn\t500\npolice\t50\n"
        self.assertEqual(self.run_clean("tl", lexicon, english),
                         ["ang\t5000", "at\t3000", "kumusta\t50"])

    def test_strongly_local_word_spelled_like_english_is_kept(self):
        # "raw" is a Tagalog particle: far more frequent here than in English.
        lexicon = "raw\t1000\nlove\t10\nako\t1000\n"
        english = "raw\t1\nlove\t1000\nfiller\t100000\n"
        self.assertEqual(self.run_clean("tl", lexicon, english), ["ako\t1000", "raw\t1000"])

    def test_waray_keep_list(self):
        lexicon = "han\t900\nha\t800\nin\t700\nof\t50\nsalticidae\t30\n"
        english = "ha\t500\nin\t9000\nof\t9000\n"
        self.assertEqual(self.run_clean("war", lexicon, english),
                         ["han\t900", "ha\t800", "in\t700"])


if __name__ == "__main__":
    unittest.main()
