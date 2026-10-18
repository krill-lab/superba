"""UI texts of the Superba GUI from the tracked locale files, so the harness finds
widgets by the same key the GUI uses (a reworded label does not break a
scenario) in English and Simplified Chinese.

    s = Strings("en"); s.t("wizard.apply") -> "Apply"; s.re("banner.armed.title")

Placeholders `{name}` and plural forms `{n|one|many}` become wildcards in re().
"""

import re
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
LOCALE_DIRS = [REPO / "crates" / c / "locales" for c in
               ("superba-gui", "superba-core", "superba-switch", "superba-cli")]


def _flatten(d, prefix, out):
    for k, v in d.items():
        key = f"{prefix}.{k}" if prefix else k
        if isinstance(v, dict):
            _flatten(v, key, out)
        else:
            out[key] = v


class Strings:
    def __init__(self, lang="en"):
        self.lang = lang
        self.table = {}
        for d in LOCALE_DIRS:
            if not d.is_dir():
                continue
            for f in sorted(d.glob(f"*{lang}.toml")):
                # gui: <area>.<lang>.toml; core etc.: <lang>.toml / core.<area>.<lang>.toml
                if not (f.name == f"{lang}.toml" or f.name.endswith(f".{lang}.toml")):
                    continue
                data = tomllib.loads(f.read_text(encoding="utf-8"))
                flat = {}
                _flatten(data, "", flat)
                for k, v in flat.items():
                    self.table.setdefault(k, v)

    def t(self, key, **kw):
        if key not in self.table:
            raise KeyError(f"no UI text {key!r} for {self.lang}")
        s = self.table[key]

        def plural(m):
            name, one, many = m.group(1), m.group(2), m.group(3)
            n = kw.get(name)
            return one if n == 1 else many

        s = re.sub(r"\{(\w+)\|([^|}]*)\|([^}]*)\}", plural, s)
        return re.sub(r"\{(\w+)\}", lambda m: str(kw.get(m.group(1), m.group(0))), s)

    def re(self, key):
        """A regex matching the text with any placeholder values (anchored)."""
        s = self.table[key]
        parts = re.split(r"(\{[^}]*\})", s)
        out = []
        for p in parts:
            if p.startswith("{") and p.endswith("}"):
                if "|" in p:
                    alts = p[1:-1].split("|")[1:]
                    out.append("(?:" + "|".join(re.escape(a) for a in alts) + ")")
                else:
                    out.append(".*?")
            else:
                out.append(re.escape(p))
        return "^" + "".join(out) + "$"

    def has(self, key):
        return key in self.table
