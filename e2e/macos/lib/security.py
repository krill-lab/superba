"""Read env values without evaluating shell code; scrub before persisting."""

import json
import re
import shlex
from pathlib import Path


def load_env(path):
    values = {}
    if not path:
        return values
    for line in Path(path).read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[7:]
        name, sep, raw = line.partition("=")
        if not sep or not re.fullmatch(r"[A-Z_][A-Z0-9_]*", name):
            raise ValueError("Invalid env assignment (values are not executed)")
        parsed = shlex.split(raw, comments=True)
        if len(parsed) > 1:
            raise ValueError("Env values containing spaces must be quoted")
        values[name] = parsed[0] if parsed else ""
    return values


class Redactor:
    def __init__(self, values=()):
        self.values = set()
        self.add(*values)

    def add(self, *values):
        self.values.update(str(value) for value in values if value and len(str(value)) >= 4)

    def text(self, value):
        text = str(value)
        for secret in sorted(self.values, key=len, reverse=True):
            text = text.replace(secret, "<redacted>")
        text = re.sub(r"\bsbs_[0-9a-f]+_[0-9a-f]+\b", "<redacted>", text)
        text = re.sub(r"\bsk-[A-Za-z0-9_-]{8,}\b", "<redacted>", text)
        text = re.sub(r'(?i)(bearer\s+)[^\s"\\]+', r"\1<redacted>", text)
        text = re.sub(
            r'(?i)(["\w-]*(?:token|api_key|auth_token|password)["\w-]*\s*[=:]\s*")[^"]*',
            r'\1<redacted>', text,
        )
        return text

    def json(self, value):
        def scrub(item):
            if isinstance(item, dict):
                return {self.text(key): ("<redacted>" if isinstance(val, str) and
                        re.search(r"(?i)(token|api_key|password)", str(key)) else scrub(val))
                        for key, val in item.items()}
            if isinstance(item, (list, tuple)):
                return [scrub(val) for val in item]
            return self.text(item) if isinstance(item, str) else item

        # Scrub nested strings before JSON escaping; otherwise escaped quote marks
        # can hide generated tokens in stdout/stderr fields.
        return json.dumps(scrub(value), ensure_ascii=False, indent=2) + "\n"
