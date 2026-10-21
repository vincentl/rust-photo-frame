#!/usr/bin/env python3
"""Tests for analyze-playlist-metrics.py's PAIRING section.

    python3 -B developer/test_analyze_playlist_metrics.py
"""

import contextlib
import importlib.util
import io
import os
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location(
    "analyze", os.path.join(HERE, "analyze-playlist-metrics.py")
)
analyze = importlib.util.module_from_spec(spec)
spec.loader.exec_module(analyze)


class Log:
    """Journal lines in the frame's format, one process at a time."""

    def __init__(self):
        self.lines = []
        self.t = 0

    def stamp(self, pid, pairing):
        extra = " pairing=true pairing_probability=1 pairing_bar_ratio=0.618 pairing_dwell_multiplier=1.5" if pairing else ""
        self.lines.append(
            f"Oct 20 frame photoframe[{pid}]: {self.ts()}  INFO playlist_scheduler order=weighted-spread "
            f"refractory=0.700 min_spacing=0.700 new_multiplicity=3 half_life_secs=86400{extra}"
        )
        self.seq, self.screen, self.shown, self.sent = 0, 0, {}, 0

    def ts(self):
        self.t += 30
        return f"2026-10-20T{12 + self.t // 3600:02d}:{self.t // 60 % 60:02d}:{self.t % 60:02d}.000000Z"

    def screen_of(self, pid, members, pairing=True, added=0, removed=0, sent=None):
        self.screen += 1
        self.sent = self.sent + 1 if sent is None else sent
        stamp = self.ts()
        for i, m in enumerate(members):
            self.seq += 1
            self.shown[m] = self.shown.get(m, 0) + 1
            fields = (
                f"seq={self.seq} inventory=9 distinct={len(self.shown)} shown_count={self.shown[m]} "
                f"gap=5 weight=1.00 age_days=30.0 created_source=birthtime"
            )
            if pairing:
                role = "solo" if len(members) == 1 else ("first" if i == 0 else "second")
                partner = f'"{members[1 - i]}"' if len(members) == 2 else '""'
                fields += (
                    f" screen={self.screen} pair={role} partner={partner} pairable=true "
                    f"sent={self.sent} removed={removed} added={added}"
                )
            self.lines.append(f"Oct 20 frame photoframe[{pid}]: {stamp}  INFO photo_display_metric {fields} path={m}")

    def run(self):
        with tempfile.TemporaryDirectory() as d:
            log, lst = os.path.join(d, "log"), os.path.join(d, "list")
            with open(log, "w") as fh:
                fh.write("\n".join(self.lines) + "\n")
            with open(lst, "w") as fh:
                fh.write("\n".join(f"/p/{c}.jpg" for c in "abcdefgh") + "\n")
            out = io.StringIO()
            argv = sys.argv
            sys.argv = ["analyze", log, lst]
            try:
                with contextlib.redirect_stdout(out):
                    analyze.main()
            finally:
                sys.argv = argv
            return out.getvalue()


class PairingSection(unittest.TestCase):
    def test_pairing_off_logs_print_no_pairing_section(self):
        # Review F2: the main-format lines of a pairing-off run.
        log = Log()
        log.stamp(100, pairing=False)
        for m in ["/p/a.jpg", "/p/b.jpg", "/p/a.jpg", "/p/c.jpg"]:
            log.screen_of(100, [m], pairing=False)
        self.assertNotIn("# PAIRING", log.run())

    def test_a_reused_pid_does_not_merge_two_sessions(self):
        # Review F3: two processes with the same PID; screen= restarts at 1.
        # Neither session puts a photo on two consecutive screens.
        log = Log()
        for _ in range(2):
            log.stamp(100, pairing=True)
            log.screen_of(100, ["/p/a.jpg", "/p/b.jpg"])
            log.screen_of(100, ["/p/c.jpg"])
            log.screen_of(100, ["/p/d.jpg", "/p/e.jpg"])
        out = log.run()
        self.assertIn("sessions=2 screens=6", out)
        self.assertIn("photos on consecutive screens: 0", out)

    def test_sessions_split_where_screen_restarts_without_a_stamp(self):
        log = Log()
        log.stamp(100, pairing=True)
        log.screen_of(100, ["/p/a.jpg", "/p/b.jpg"])
        log.screen_of(100, ["/p/c.jpg"])
        log.lines = log.lines[1:]  # the stamp fell outside the gathered span
        log.screen = 0
        log.screen_of(100, ["/p/c.jpg", "/p/d.jpg"])
        out = log.run()
        self.assertIn("sessions=2", out)
        self.assertIn("photos on consecutive screens: 0", out)

    def test_repeat_after_an_add_is_attributed_and_a_bare_repeat_is_not(self):
        # Review F6: an add re-peeks the held photo; (P, X), (P) can follow.
        log = Log()
        log.stamp(100, pairing=True)
        log.shown = {f"/p/{c}.jpg": 3 for c in "abcdef"}  # none is a debut
        log.screen_of(100, ["/p/a.jpg", "/p/b.jpg"])
        log.screen_of(100, ["/p/c.jpg", "/p/d.jpg"])
        log.screen_of(100, ["/p/c.jpg"], added=1)
        log.screen_of(100, ["/p/e.jpg"])
        log.screen_of(100, ["/p/e.jpg", "/p/f.jpg"])
        out = log.run()
        self.assertIn("1  inventory change (add or removal) between planning and display", out)
        self.assertIn("1  UNATTRIBUTED", out)


if __name__ == "__main__":
    unittest.main()
