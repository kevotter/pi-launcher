# Ciallo Serif — startup page font

`ciallo-700.woff2` and `ciallo-800.woff2` are a **subset** of
[Source Han Serif SC](https://github.com/adobe-fonts/source-han-serif) (思源宋体) by
Adobe — Copyright 2017-2022 Adobe, <http://www.adobe.com/>, with Reserved Font
Name "Source" — licensed under the SIL Open Font License 1.1. The full licence
text is [OFL.txt](OFL.txt) next to these files.

- Only the glyphs of the string in `ui/loading.js` are kept (16 code points
  including the space). The outlines are otherwise unmodified.
- A subset is a "Modified Version" under the OFL, which forbids reusing the
  Reserved Font Name, so the family is `Ciallo Serif SC` in the font's own name
  table and the page declares the CSS family `Ciallo Serif`.
- The copyright, trademark and licence statements inside the font are kept
  verbatim. Do not strip them.
- These files are **not** MIT-licensed like the rest of this repository; they
  stay under the OFL and the licence must travel with any redistribution. Vite
  folds `OFL.txt` into the built `THIRD_PARTY_FRONTEND_LICENSES.txt`.
- Regenerate with `scripts/build-startup-font.py` after editing that string —
  never hand-edit these binaries.

Attribution is also recorded in the repository-level
[THIRD_PARTY_NOTICES.md](../../THIRD_PARTY_NOTICES.md) and the README.
