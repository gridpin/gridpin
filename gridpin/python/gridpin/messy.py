"""Addresses as people write them: the distortion classes of the GridPin test sets, with a country profile.

`messy("Bulevar Oslobođenja 100, 21000 Novi Sad", seed=1)` returns the address in one distorted form: a type word
dropped or shortened, the last segment moved to the front, two letters swapped, the postcode glued to the town,
diacritics stripped, Cyrillic transliterated, commas removed, a flat or a phone added, lower case. `profile` gives the
share of each class (`{"typo_transposition": 2, "comma_removal_whitespace": 1}`); a class with share 0 is never
used; the default gives every class the same share. Pure functions, no dependencies, the same seed gives the same
output. `apply_form` is the exact form of one class as the distorted test sets of the engine use it.

Limits: the flat and phone noise is the same in every country (`GENERIC`), Cyrillic is transliterated the Serbian way
only, a typo can move a capital letter inside a word ("aPris"), "last segment first" also moves a trailing country to
the front, and an empty string comes back with noise.
"""
from __future__ import annotations

import dataclasses
import hashlib
import random
import re
import unicodedata
from typing import Dict, Optional, Tuple

__all__ = ["messy", "CLASSES", "Profile", "apply_form", "latin"]

FreezeError = ValueError

POSTCODE5_RE = re.compile(r"(?<!\d)\d{5}(?!\d)")
NL_POSTCODE_RE = re.compile(r"(?<!\d)(\d{4})( ?)([A-Z]{2})(?!\w)")
CYRILLIC = dict(zip("абвгдђежзијклљмнњопрстћуфхцчџш",
                    ["a", "b", "v", "g", "d", "đ", "e", "ž", "z", "i", "j", "k", "l", "lj", "m", "n", "nj",
                     "o", "p", "r", "s", "t", "ć", "u", "f", "h", "c", "č", "dž", "š"]))


@dataclasses.dataclass(frozen=True)
class Profile:
    prefix: str
    units: tuple
    phone: str
    abbreviations: dict
    suffix_types: bool = False

    def type_re(self):
        words = "|".join(sorted(self.abbreviations, key=len, reverse=True))
        if self.suffix_types:   # Dutch: the type ends the word («Kerkstraat»)
            return re.compile(r"(?<=\w)(" + words + r")(?!\w)", re.I)
        return re.compile(r"(?<!\w)(" + words + r")(?!\w)", re.I)


def latin(text):
    out = []
    for ch in text:
        low = ch.lower()
        if low in CYRILLIC:
            rep = CYRILLIC[low]
            out.append(rep.capitalize() if ch != low else rep)
        else:
            out.append(ch)
    return "".join(out)


def apply_form(profile, domain, raw, row_id, ordinal, name):
    digest = hashlib.sha256(domain + row_id.encode()).digest()
    if name == "recipient_prefix":
        return profile.prefix + raw, "recipient_prefix"
    if name == "unit_or_phone_noise_suffix":
        if digest[1] % 2 == 0:
            suffix = profile.units[digest[2] % 2]
            return f"{raw}, {suffix}", "unit_noise:" + suffix
        number = f"{10_000_000 + ordinal * 100:08d}"
        return f"{raw}, " + profile.phone.format(a=number[:2], b=number[2:4], c=number[4:6], d=number[6:]), "phone_noise"
    if name == "comma_removal_whitespace":
        if "," not in raw:
            return None, "inapplicable_no_comma"
        return " ".join(raw.replace(",", " ").split()), "commas_removed_whitespace_collapsed"
    if name == "tail_locality_first":
        if "," not in raw:
            return None, "inapplicable_no_comma"
        body, tail = raw.rsplit(",", 1)
        if not body.strip() or not tail.strip():
            return None, "inapplicable_empty_comma_segment"
        return f"{tail.strip()}, {body.strip()}", "last_comma_segment_moved_to_front"
    if name == "diacritics_stripped":
        result = "".join(c for c in unicodedata.normalize("NFD", raw) if not unicodedata.combining(c))
        return (result, "diacritics_removed") if result != raw else (None, "inapplicable_no_diacritics")
    if name == "street_type_abbreviation":
        match = profile.type_re().search(raw)
        if match is None:
            return None, "inapplicable_no_full_street_type"
        return (raw[:match.start()] + profile.abbreviations[match.group().casefold()] + raw[match.end():],
                "street_type:" + match.group().casefold())
    if name == "postcode_dropped":
        match = POSTCODE5_RE.search(raw)
        if match is None:
            return None, "inapplicable_no_postcode"
        result = " ".join((raw[:match.start()] + raw[match.end():]).split()).strip(" ,").replace(" ,", ",")
        return (result, "postcode_removed") if result else (None, "inapplicable_postcode_only")
    if name == "postcode_space_toggled":
        match = NL_POSTCODE_RE.search(raw)
        if match is None:
            return None, "inapplicable_no_postcode"
        glued = match.group(1) + ("" if match.group(2) else " ") + match.group(3)
        return raw[:match.start()] + glued + raw[match.end():], "postcode_space_toggled"
    if name == "case_lowered":
        result = raw.lower()
        return (result, "lowercased") if result != raw else (None, "inapplicable_already_lowercase")
    if name == "cyrillic_to_latin":
        result = latin(raw)
        return (result, "cyrillic_transliterated") if result != raw else (None, "inapplicable_no_cyrillic")
    raise FreezeError(f"unknown class: {name}")


# Free generator -----------------------------------------------------------------------------------------------

GENERIC = Profile(
    prefix="Attn: ",
    units=("apt 2", "floor 3"),
    phone="+00 {a} {b}{c}{d}",
    abbreviations={"street": "St", "avenue": "Ave", "road": "Rd", "boulevard": "Blvd", "rue": "r.", "calle": "c/",
                   "ulica": "ul.", "bulevar": "bul.", "straße": "str.", "strasse": "str.", "via": "v.",
                   "avenida": "av.", "rua": "r."},
)

CLASSES = (
    "street_type_dropped",
    "street_type_abbreviation",
    "tail_locality_first",
    "typo_transposition",
    "postcode_glued_to_locality",
    "diacritics_stripped",
    "cyrillic_to_latin",
    "comma_removal_whitespace",
    "unit_or_phone_noise_suffix",
    "case_lowered",
)

# A postcode that opens a comma segment and is followed by the town («…, 12043 Berlin»). Not the first segment: there a
# number before a word is the house number («1023 Fourth Ave, San Diego, CA 92101»), and gluing it to the street is
# not the postcode glued to the town (revision 0.9.0: US 57 of 57, AU 8 of 8).
_POSTCODE_ANY = re.compile(r",\s*(\d{4,5}(?:-\d{3})?)\s+([^\W\d]{2,})")


def _extra(name: str, raw: str, rng: random.Random, profile: Profile) -> Tuple[Optional[str], str]:
    if name == "street_type_dropped":
        match = profile.type_re().search(raw)
        if match is None:
            return None, "inapplicable_no_full_street_type"
        start = match.start()
        if start > 0 and raw[start - 1] == "-":
            start -= 1   # «Karl-Marx-Straße 12» -> «Karl-Marx 12», not «Karl-Marx- 12» (revision 0.9.0: DE 15 of 16)
        out = " ".join((raw[:start] + raw[match.end():]).split())
        return out, "street_type_dropped:" + match.group().casefold()
    if name == "typo_transposition":
        words = [(m.start(), m.end()) for m in re.finditer(r"[^\W\d_]{4,}", raw)]
        if not words:
            return None, "inapplicable_no_long_word"
        start, end = words[rng.randrange(len(words))]
        i = rng.randrange(start, end - 1)
        return raw[:i] + raw[i + 1] + raw[i] + raw[i + 2:], "letters_swapped"
    if name == "postcode_glued_to_locality":
        match = _POSTCODE_ANY.search(raw)
        if match is None:
            return None, "inapplicable_no_postcode_before_locality"
        return (raw[:match.start(1)] + match.group(1) + match.group(2) + raw[match.end():], "postcode_glued")
    return apply_form(profile, b"messy", raw, str(rng.random()), rng.randrange(1000), name)


def messy(address: str, profile: Optional[Dict[str, float]] = None, seed: Optional[int] = None,
          country: Optional[Profile] = None) -> Tuple[str, str]:
    """`address` in one distorted form: (text, class). Classes that do not apply to this address are skipped in the
    order the profile's weights draw them; when none applies, the address comes back unchanged with class "none"."""
    weights = {c: 1.0 for c in CLASSES} if profile is None else {c: float(profile.get(c, 0)) for c in CLASSES}
    unknown = set(profile or {}) - set(CLASSES)
    if unknown:
        raise ValueError(f"unknown classes: {sorted(unknown)}; known: {', '.join(CLASSES)}")
    rng = random.Random(seed)
    country = country or GENERIC
    left = {c: w for c, w in weights.items() if w > 0}
    while left:
        names = sorted(left)
        name = rng.choices(names, weights=[left[n] for n in names])[0]
        out, _ = _extra(name, address, rng, country)
        if out is not None and out != address:
            return out, name
        del left[name]
    return address, "none"
