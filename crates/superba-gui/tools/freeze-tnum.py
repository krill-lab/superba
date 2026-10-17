#!/usr/bin/env python3
"""Make tabular figures the default in the bundled Geist fonts.

egui cannot enable OpenType features, so the `tnum` substitution is applied to the
font itself: every character whose glyph `tnum` replaces is remapped in the cmap to
the tabular glyph. Numbers in tables, countdowns, latencies and money then line up.

Requires fontTools (`pip install fonttools`). Run from the repository root:

    python3 crates/superba-gui/tools/freeze-tnum.py crates/superba-gui/assets/fonts/Geist-*.ttf

Idempotent; the SIL OFL 1.1 permits modified versions (Geist declares no Reserved
Font Name). The font's name table gets a note in its version string.
"""
import sys

from fontTools.ttLib import TTFont


def tnum_map(font):
    gsub = font["GSUB"].table
    lookups = set()
    for rec in gsub.FeatureList.FeatureRecord:
        if rec.FeatureTag == "tnum":
            lookups.update(rec.Feature.LookupListIndex)
    mapping = {}
    for i in sorted(lookups):
        lookup = gsub.LookupList.Lookup[i]
        for sub in lookup.SubTable:
            if lookup.LookupType == 7:  # extension
                sub = sub.ExtSubTable
            if hasattr(sub, "mapping"):
                mapping.update(sub.mapping)
    return mapping


def freeze(path):
    font = TTFont(path)
    mapping = tnum_map(font)
    if not mapping:
        print(f"{path}: no tnum substitutions")
        return
    changed = 0
    for table in font["cmap"].tables:
        for code, glyph in list(table.cmap.items()):
            if glyph in mapping:
                table.cmap[code] = mapping[glyph]
                changed += 1
    for rec in font["name"].names:
        if rec.nameID == 5:
            text = rec.toUnicode()
            if "tnum" not in text:
                rec.string = text + "; tnum default (Superba)"
    font.save(path)
    hmtx = font["hmtx"]
    cmap = font.getBestCmap()
    widths = {hmtx[cmap[ord(c)]][0] for c in "0123456789"}
    print(f"{path}: remapped {changed} cmap entries; digit widths {sorted(widths)}")


if __name__ == "__main__":
    for p in sys.argv[1:]:
        freeze(p)
