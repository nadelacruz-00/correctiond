import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "build-lexicon.py")


def run(args, files):
    with tempfile.TemporaryDirectory() as d:
        paths = {}
        for name, content in files.items():
            p = os.path.join(d, name)
            with open(p, "w", encoding="utf-8") as f:
                f.write(content)
            paths[name] = p
        out = os.path.join(d, "out.txt")
        cmd = [sys.executable, SCRIPT] + [paths.get(a, a) for a in args] + ["--out", out]
        subprocess.run(cmd, check=True, capture_output=True)
        with open(out, encoding="utf-8") as f:
            lines = [l for l in f.read().splitlines() if l and not l.startswith("#")]
        mode = os.stat(out).st_mode & 0o777
        return lines, mode


class BuildLexiconTest(unittest.TestCase):
    def test_leipzig_min_count(self):
        text = "1\tAko\t500\n2\tKAMI\t40\n3\tbata1\t9\n4\tñino\t8\n5\trare\t2\n"
        lines, mode = run(
            ["--lang", "tl", "--freq", "f.txt", "--freq-format", "leipzig", "--min-count", "3"],
            {"f.txt": text},
        )
        self.assertEqual(lines, ["ako\t500", "kami\t40"])
        self.assertEqual(mode, 0o644)

    def test_leipzig_four_column_community_format(self):
        # The 2017 community downloads (tgl_community_2017, war_community_2017)
        # write `<id>\t<word>\t<word>\t<count>`; the count is the last field.
        text = "1\t!\t!\t13\n101\tnga\tnga\t837503\n108\tAn\tAn\t753383\n109\thadton\thadton\t2\n"
        lines, _ = run(
            ["--lang", "war", "--freq", "f.txt", "--freq-format", "leipzig", "--min-count", "3"],
            {"f.txt": text},
        )
        self.assertEqual(lines, ["nga\t837503", "an\t753383"])

    def test_multiple_freq_files_are_summed(self):
        # --freq may repeat (e.g. a 2017 community corpus plus a 2021
        # Wikipedia sample, mixing the 4- and 3-column forms); counts add up
        # before --min-count applies.
        lines, _ = run(
            ["--lang", "tl", "--freq", "a.txt", "--freq", "b.txt", "--min-count", "3"],
            {
                "a.txt": "1\tako\tako\t500\n2\tbuong\tbuong\t2\n",
                "b.txt": "1\tako\t5\n2\tbuong\t1\n3\tkasama\t2\n",
            },
        )
        self.assertEqual(lines, ["ako\t505", "buong\t3"])

    def test_words_set_plain(self):
        lines, _ = run(
            ["--lang", "en", "--freq", "f.txt", "--freq-format", "plain", "--words", "w.txt"],
            {"f.txt": "the 1000\nbuld 9\n", "w.txt": "build\nthe\n"},
        )
        self.assertEqual(sorted(lines), ["build\t1", "the\t1000"])

    def test_non_ascii_digit_count_is_skipped(self):
        # str.isdigit accepts superscripts, which int() then rejects.
        text = "1\tako\t500\n2\tkami\t\u00b2\n3\tbata\t\u0664\n4\tsila\t12\n"
        lines, _ = run(
            ["--lang", "tl", "--freq", "f.txt", "--freq-format", "leipzig", "--min-count", "1"],
            {"f.txt": text},
        )
        self.assertEqual(lines, ["ako\t500", "sila\t12"])

    def test_creates_missing_parent_dirs_0700(self):
        with tempfile.TemporaryDirectory() as d:
            src = os.path.join(d, "f.txt")
            with open(src, "w", encoding="utf-8") as f:
                f.write("1\tnaman\t50\n")
            out = os.path.join(d, "share", "correctiond", "lexicons", "tl.txt")
            subprocess.run(
                [sys.executable, SCRIPT, "--lang", "tl", "--freq", src, "--out", out],
                check=True, capture_output=True,
            )
            with open(out, encoding="utf-8") as f:
                self.assertIn("naman\t50\n", f.read())
            for sub in ["share", "share/correctiond", "share/correctiond/lexicons"]:
                mode = os.stat(os.path.join(d, sub)).st_mode & 0o777
                self.assertEqual(mode, 0o700, sub)
            self.assertEqual(os.stat(out).st_mode & 0o777, 0o644)

    def test_min_count_help_says_it_does_not_apply_with_words(self):
        r = subprocess.run([sys.executable, SCRIPT, "--help"],
                           check=True, capture_output=True, text=True)
        self.assertIn("ignored with --words", " ".join(r.stdout.split()))


class FreqSpecModeTest(unittest.TestCase):
    def build(self, files, argv, expect_ok=True):
        with tempfile.TemporaryDirectory() as d:
            for name, content in files.items():
                with open(os.path.join(d, name), "w", encoding="utf-8") as f:
                    f.write(content)
            out = os.path.join(d, "out.txt")
            argv = [a.replace("@", d + os.sep) for a in argv]
            r = subprocess.run([sys.executable, SCRIPT] + argv + ["--out", out],
                               capture_output=True, text=True)
            if not expect_ok:
                return r
            self.assertEqual(r.returncode, 0, r.stderr)
            with open(out, encoding="utf-8") as f:
                lines = [l for l in f.read().splitlines() if l and not l.startswith("#")]
            return lines, os.stat(out).st_mode & 0o777

    def test_mixed_formats_in_one_run_are_summed(self):
        lines, _ = self.build(
            {"l.txt": "1\tako\t10\n", "p.txt": "ako 5\n", "x.txt": "# c\nako\t7\nbuo\t4\n"},
            ["--lang", "tl", "--freq", "leipzig:@l.txt", "--freq", "plain:@p.txt",
             "--freq", "lexicon:@x.txt"],
        )
        self.assertEqual(lines, ["ako\t22", "buo\t4"])

    def test_path_with_colon_is_not_a_format_prefix(self):
        lines, _ = self.build(
            {"c:d.txt": "1\tako\t10\n", "e:f.txt": "ako\t3\n"},
            ["--lang", "tl", "--freq", "@c:d.txt", "--freq", "lexicon:@e:f.txt"],
        )
        self.assertEqual(lines, ["ako\t13"])

    def test_legacy_freq_format_still_applies_to_bare_paths(self):
        lines, mode = self.build(
            {"p.txt": "ako 5\n"},
            ["--lang", "tl", "--freq", "@p.txt", "--freq-format", "plain", "--min-count", "3"],
        )
        self.assertEqual(lines, ["ako\t5"])
        self.assertEqual(mode, 0o644)

    def test_mode_applies_to_output(self):
        _, mode = self.build(
            {"p.txt": "ako 5\n"},
            ["--lang", "tl", "--freq", "plain:@p.txt", "--mode", "0600"],
        )
        self.assertEqual(mode, 0o600)

    def test_invalid_mode_is_rejected(self):
        for bad in ["abc", "0999", "07777", "-1", ""]:
            r = self.build({"p.txt": "ako 5\n"},
                           ["--lang", "tl", "--freq", "plain:@p.txt", "--mode", bad],
                           expect_ok=False)
            self.assertEqual(r.returncode, 2, bad)
            self.assertIn("--mode", r.stderr)


if __name__ == "__main__":
    unittest.main()
