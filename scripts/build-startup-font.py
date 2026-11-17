"""Build the startup page's Ciallo wordmark subset.

The animation only ever draws "Ciallo～(∠・ω< )⌒★", so shipping a full CJK
serif (25 MB per weight) would be absurd. This cuts the two weights the page
uses down to the code points that string needs, then renames the family: a
subset is a Modified Version under the OFL, which forbids keeping the Reserved
Font Name "Source". The copyright, trademark and licence statements inside the
font are left untouched, and `ui/fonts/OFL.txt` is what makes the redistribution
compliant — the attribution text in README.md, THIRD_PARTY_NOTICES.md and
ui/fonts/README.md describes the same thing and must stay in sync.

Run with a Python that has fonttools + brotli, e.g.
  py -3 -m pip install fonttools brotli
  py -3 scripts/build-startup-font.py
"""

import pathlib
import re

from fontTools.subset import Options, Subsetter
from fontTools.ttLib import TTFont

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = pathlib.Path.home() / "AppData/Local/Microsoft/Windows/Fonts"
OUT = ROOT / "ui/fonts"

WEIGHTS = {"Bold": 700, "Heavy": 800}


def ciallo_text() -> str:
    """Read the animated string, so a copy edit cannot leave the subset behind."""
    source = (ROOT / "ui/loading.js").read_text(encoding="utf-8")
    match = re.search(r"const TEXT = '([^']+)'", source)
    if not match:
        raise SystemExit("cannot find TEXT in ui/loading.js")
    return match.group(1)


UNICODES = sorted({ord(char) for char in ciallo_text()})


def build(style: str) -> None:
    source = SRC / f"SourceHanSerifSC-{style}.otf"
    if not source.is_file():
        raise SystemExit(f"missing {source}; install Source Han Serif SC first")

    font = TTFont(source)

    options = Options()
    options.layout_features = []
    options.hinting = False
    options.desubroutinize = True
    options.name_IDs = ["*"]
    options.name_legacy = True
    options.name_languages = ["*"]
    options.notdef_outline = True
    options.recalc_bounds = True
    options.drop_tables.append("DSIG")

    subsetter = Subsetter(options=options)
    subsetter.populate(unicodes=UNICODES)
    subsetter.subset(font)

    for record in font["name"].names:
        try:
            text = record.toUnicode()
        except UnicodeDecodeError:
            continue
        renamed = text.replace("Source Han Serif", "Ciallo Serif").replace("SourceHanSerif", "CialloSerif")
        if renamed != text:
            record.string = renamed

    font.flavor = "woff2"
    target = OUT / f"ciallo-{WEIGHTS[style]}.woff2"
    font.save(target)
    print(f"{target.relative_to(ROOT)}  {target.stat().st_size} bytes")


for weight in WEIGHTS:
    build(weight)
