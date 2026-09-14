#!/usr/bin/env python3
"""Every shipped icon must fill its own canvas.

The complaint that started this was "the size of our icon is always too small",
and it was not a matter of taste: the cloud spanned 50% of its 128x128 viewBox
by 39%, centred eleven pixels below the middle. Launchers and panels scale the
canvas, not the drawing, so a mark that leaves half its canvas empty renders at
half size everywhere and there is no setting that fixes it.

That is measurable, so it is measured here rather than left for someone to
notice on a panel months later. Each icon is rendered and its drawn extent --
the bounding box of everything non-transparent -- is compared with its canvas.

Needs rsvg-convert and ImageMagick, which the contributor checks already rely
on; it skips rather than fails if they are missing, so a machine without them
is not blocked.
"""

import re
import shutil
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ICONS = sorted((ROOT / "packaging/icons").rglob("*.svg"))

# A mark should reach most of the way to its edges. 82% leaves room for the
# rounded corners of a tile and the optical inset a round shape wants, while
# still failing anything that sits in the middle of an empty square.
MIN_FILL = 0.82
# And it should sit in the middle: a tenth of the canvas off-centre is a mark
# that looks wrong beside its neighbours in a panel.
MAX_OFFSET = 0.10
RENDER = 256


# ImageMagick 7 calls it magick; 6, which is what Ubuntu 24.04 still ships,
# calls it convert. Taking either is the difference between this running in CI
# and quietly skipping there, which would look exactly like passing.
IMAGEMAGICK = shutil.which("magick") or shutil.which("convert")


def extent(svg):
    """(width, height, cx, cy) of the drawn pixels, as fractions of the canvas."""
    png = subprocess.run(
        ["rsvg-convert", "-w", str(RENDER), "-h", str(RENDER), str(svg)],
        capture_output=True, check=True,
    ).stdout
    out = subprocess.run(
        [IMAGEMAGICK, "png:-", "-trim", "-format", "%w %h %X %Y", "info:"],
        input=png, capture_output=True, check=True,
    ).stdout.decode().split()
    w, h = int(out[0]), int(out[1])
    # %X/%Y carry a sign, e.g. "+12".
    x, y = int(out[2]), int(out[3])
    return (w / RENDER, h / RENDER,
            (x + w / 2) / RENDER, (y + h / 2) / RENDER)


SYMBOLIC = sorted((ROOT / "packaging/icons/symbolic").rglob("*.svg"))


@unittest.skipUnless(
    shutil.which("rsvg-convert") and IMAGEMAGICK,
    "needs rsvg-convert and ImageMagick",
)
class IconsFillTheirCanvas(unittest.TestCase):
    def test_every_icon_reaches_its_edges_and_sits_in_the_middle(self):
        self.assertTrue(ICONS, "no icons found; the path has moved")
        small, offset = [], []
        for svg in ICONS:
            w, h, cx, cy = extent(svg)
            name = svg.relative_to(ROOT)
            if w < MIN_FILL or h < MIN_FILL:
                small.append(f"{name}: fills {w:.0%} x {h:.0%}, wanted {MIN_FILL:.0%}")
            if abs(cx - 0.5) > MAX_OFFSET or abs(cy - 0.5) > MAX_OFFSET:
                offset.append(f"{name}: centred at {cx:.0%},{cy:.0%} of the canvas")
        self.assertEqual(small, [], "these icons leave their canvas half empty, so every "
                                    "launcher and panel draws them small:\n  " + "\n  ".join(small))
        self.assertEqual(offset, [], "these icons do not sit in the middle of their canvas:\n  "
                                     + "\n  ".join(offset))


class SymbolicIconsAreFilledPaths(unittest.TestCase):
    """A symbolic icon must be filled paths, never strokes.

    GTK recolours symbolic icons through its own pipeline, and a stroke does
    not survive it. The Cirrove mark was first drawn as a stroked ring with a
    filled centre dot; it rendered perfectly with rsvg at every size and came
    out in the settings window as one solid blue disc, the ring having closed
    over its own hole. Every Adwaita symbolic icon is filled paths, which is
    the convention this follows rather than discovers.

    Scoped to the symbolic directory on purpose: the tray and application icons
    are drawn by a shell as ordinary icons, strokes and all, and are verified
    working that way in GNOME and in Quickshell.
    """

    def test_no_symbolic_icon_relies_on_a_stroke(self):
        self.assertTrue(SYMBOLIC, "no symbolic icons found; the path has moved")
        stroked = []
        for svg in SYMBOLIC:
            text = svg.read_text()
            # stroke="none" is the harmless form that says "fill only".
            for match in re.findall(r'stroke\s*=\s*"([^"]*)"', text):
                if match.strip().lower() not in ("none", ""):
                    stroked.append(f"{svg.relative_to(ROOT)}: stroke=\"{match}\"")
            if re.search(r'stroke-width\s*=', text):
                stroked.append(f"{svg.relative_to(ROOT)}: has stroke-width")
        self.assertEqual(
            stroked, [],
            "GTK flattens a stroked symbolic icon when it recolours it -- draw the "
            "shape as a filled path instead:\n  " + "\n  ".join(stroked),
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
