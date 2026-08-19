#!/usr/bin/env python3
"""Verify a downloaded GridPin release: owner signature + one checksum graph.

This script ships WITH the release so that a recipient can check what they downloaded without
trusting the download path. It is self-contained: the only external tool it needs is `ssh-keygen`.

What it checks
  1. the owner's signature over `attestation.json` (`ssh-keygen -Y verify`, using the published
     allowed-signers file);
  2. `SHA256SUMS` covers exactly the files sitting next to it — nothing missing, nothing extra;
  3. every file present is measured and compared with the signed record as a WHOLE TRIPLE
     (size, sha256, blake2b_256), and a sheet record must agree with its asset record, so the
     checksums and the signature describe the same bytes and the document cannot contradict itself.

What it does not check: the provenance of the private build inputs (the attestation is the owner's
signed statement about them) and whether a sheet answers correctly — run `gridpin` for that.

    python3 verify_public_release.py --dir <downloaded release directory>
"""
from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import shutil
import subprocess
import sys

SIGN_NAMESPACE = "gridpin-g02"
SIGN_PRINCIPAL = "gridpin-release"
SUMS_NAME = "SHA256SUMS"
ATTESTATION_NAME = "attestation.json"
SIGNERS_NAME = "gridpin-release-signers"


def sha256_file(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(1 << 16):
            h.update(chunk)
    return h.hexdigest()


def file_digests(path: pathlib.Path) -> tuple[int, str, str]:
    """(size, sha256, blake2b_256) in ONE pass over the file.

    The signed record carries all three, and only sha256 was compared with the bytes: a validly
    signed `size: -1` and a blake2b of zeros both passed while the summary line still said
    "checksums and attestation agree". The lab verifier compares the whole
    triple, so this one must too — otherwise the two sides disagree on what a valid release is, and
    the weaker side decides.
    """
    sha, blake, size = hashlib.sha256(), hashlib.blake2b(digest_size=32), 0
    with open(path, "rb") as f:
        while chunk := f.read(1 << 16):
            sha.update(chunk)
            blake.update(chunk)
            size += len(chunk)
    return size, sha.hexdigest(), blake.hexdigest()


def signature_problems(directory: pathlib.Path, signers: pathlib.Path) -> list[str]:
    """The signature comes first: without it the attestation is an ordinary editable JSON file
    and every other check below is worthless."""
    att = directory / ATTESTATION_NAME
    sig = directory / (ATTESTATION_NAME + ".sig")
    if not att.is_file():
        return [f"{ATTESTATION_NAME} is missing: this release carries no provenance attestation"]
    if not sig.is_file():
        return [f"{sig.name} is missing: the attestation is unsigned"]
    if not signers.is_file():
        return [f"allowed-signers file {signers} is missing: nothing to verify the signature against"]
    keygen = shutil.which("ssh-keygen")
    if keygen is None:
        return ["ssh-keygen not found: cannot verify the signature (this is a STOP, not a skip)"]
    with open(att, "rb") as data:
        proc = subprocess.run(
            [keygen, "-Y", "verify", "-f", str(signers), "-I", SIGN_PRINCIPAL,
             "-n", SIGN_NAMESPACE, "-s", str(sig)],
            stdin=data, capture_output=True, text=True)
    if proc.returncode != 0:
        return [f"signature over {ATTESTATION_NAME} did NOT verify: "
                f"{(proc.stderr or proc.stdout).strip()[:200]}"]
    return []


def _is_hex(value: object, width: int) -> bool:
    """Strictly a hex string of that width. `str(value)` used to be applied first, so a non-string
    could sneak through by stringifying."""
    return isinstance(value, str) and len(value) == width and \
        all(c in "0123456789abcdef" for c in value)


def _parse_sums(text: str) -> tuple[dict[str, str], list[str]]:
    """Parse SHA256SUMS strictly: hex alphabet, no duplicate names, no "last line wins".

    The old parser assigned into a dict, so a second line for the same name silently overwrote the
    first, and only the LENGTH of the digest was checked. A file could therefore be green here and
    red for an ordinary `sha256sum -c` — the contract was ambiguous.
    """
    listed: dict[str, str] = {}
    problems: list[str] = []
    for line in text.splitlines():
        if not line.strip():
            continue
        sha, sep, name = line.partition("  ")
        if not sep or not name or not _is_hex(sha, 64):
            problems.append(f"{SUMS_NAME}: unparsable line {line[:60]!r}")
            continue
        if name in listed:
            problems.append(f"{name}: listed twice in {SUMS_NAME} — a duplicate line makes the "
                            f"manifest ambiguous and lets a later line override an earlier one")
            continue
        listed[name] = sha
    return listed, problems


def sums_problems(directory: pathlib.Path, expected_here: set[str] | None = None
                  ) -> tuple[list[str], dict[str, str]]:
    """An exact set in both directions — but only for the half that is actually here.

    `SHA256SUMS` describes the WHOLE release, and the release is delivered over two channels: the
    sheets from dl.gridpin.dev, the binaries from the GitHub release. Requiring every listed line to
    exist locally therefore made BOTH halves fail even
    though the file itself was correct. `expected_here` is the set of names this channel must carry,
    derived from the SIGNED graph — so the filter cannot be loosened by editing the checksum file.

    Lines outside that set are still checked as far as they can be: the hash must match the signed
    graph (done by `asset_graph_problems`), but the bytes need not be present locally.
    """
    sums = directory / SUMS_NAME
    if not sums.is_file():
        return [f"{SUMS_NAME} is missing"], {}
    listed, problems = _parse_sums(sums.read_text(encoding="utf-8"))
    for name, sha in listed.items():
        if expected_here is not None and name not in expected_here:
            continue                    # the other half; its bytes are not expected here
        p = directory / name
        if not p.is_file():
            problems.append(f"{name}: listed in {SUMS_NAME} but not downloaded")
        elif sha256_file(p) != sha:
            problems.append(f"{name}: sha256 does not match {SUMS_NAME}")
    present = {p.name for p in directory.iterdir()
               if p.is_file() and p.name not in (SUMS_NAME, SIGNERS_NAME)}
    for extra in sorted(present - set(listed)):
        problems.append(f"{extra}: present in the directory but NOT listed in {SUMS_NAME}")
    # A missing asset of THIS half is caught by `asset_graph_problems` ("signed but not present"):
    # it judges the signed graph rather than the checksum file. Duplicating the check here bought
    # nothing — no mutation could kill the extra branch, so it proved nothing.
    return problems, listed


def attestation_problems(directory: pathlib.Path, listed: dict[str, str],
                         channel: str = "all") -> list[str]:
    """The signature and the checksums must describe the same bytes; otherwise one set could be
    signed and a different one shipped, and each check would pass on its own."""
    att = directory / ATTESTATION_NAME
    try:
        doc = json.loads(att.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as e:
        return [f"{ATTESTATION_NAME} does not parse ({e})"]
    sheets = doc.get("sheets")
    if not isinstance(sheets, list) or not sheets:
        return [f"{ATTESTATION_NAME}: no sheet records"]
    problems: list[str] = []
    if channel == "github":
        return problems     # sheets travel on the other channel; the r2 side judges them
    for rec in sheets:
        name = str(rec.get("public_name", "?"))
        want = str(rec.get("sha256", ""))
        p = directory / name
        if not p.is_file():
            problems.append(f"{name}: attested but not present in the directory")
            continue
        got = sha256_file(p)
        if got != want:
            problems.append(f"{name}: bytes do not match the ATTESTATION "
                            f"({got[:16]}... vs {want[:16]}...)")
        if name in listed and listed[name] != want:
            problems.append(f"{name}: {SUMS_NAME} and the attestation describe DIFFERENT bytes")
        if name not in listed:
            problems.append(f"{name}: attested but not listed in {SUMS_NAME}")
    return problems


PUBLIC_SHEETS = ("france.bin", "italy.bin", "netherlands.bin", "serbia.bin", "fr_poi.bin")
SCHEMA = "gridpin-release-attestation"
SCHEMA_VERSION = 3
UNSIGNABLE = (ATTESTATION_NAME, ATTESTATION_NAME + ".sig", SUMS_NAME, SIGNERS_NAME)


def load_attestation(directory: pathlib.Path) -> tuple[dict | None, list[str]]:
    try:
        return json.loads((directory / ATTESTATION_NAME).read_text(encoding="utf-8")), []
    except (OSError, json.JSONDecodeError) as e:
        return None, [f"{ATTESTATION_NAME} does not parse ({e})"]


def shape_problems(doc: dict, expected_public_sha: str) -> list[str]:
    """The signed document must name THIS release and cover the whole public sheet set.

    Without the expected commit an old, still validly signed attestation is accepted for a new tag:
    the signature stays valid forever, so only the binding to a concrete snapshot makes it specific.
    """
    problems: list[str] = []
    if doc.get("schema") != SCHEMA or doc.get("version") != SCHEMA_VERSION:
        problems.append(f"attestation schema/version {doc.get('schema')!r}/{doc.get('version')!r} "
                        f"is not {SCHEMA}/{SCHEMA_VERSION}")
    sha = str(doc.get("public_snapshot_sha", ""))
    if len(sha) != 40 or any(c not in "0123456789abcdef" for c in sha):
        problems.append(f"public_snapshot_sha={sha!r} is not a 40-hex commit: the attestation is "
                        f"not bound to a concrete release")
    want = (expected_public_sha or "").lower().strip()
    if len(want) != 40 or any(c not in "0123456789abcdef" for c in want):
        problems.append("--expected-public-sha is required and must be a 40-hex commit: without it "
                        "an older signed attestation would be accepted for this release")
    elif sha != want:
        problems.append(f"the attestation was issued for {sha[:12]}... but this release is "
                        f"{want[:12]}...: stale or foreign document")
    names = [str(r.get("public_name")) for r in doc.get("sheets", []) if isinstance(r, dict)]
    if len(names) != len(set(names)):
        problems.append("duplicate sheet records: the document does not say which one is real")
    missing = [n for n in PUBLIC_SHEETS if n not in names]
    if missing:
        problems.append(f"the attestation does not cover {missing}: a truncated attestation would "
                        f"leave those sheets unchecked")
    if not isinstance(doc.get("assets"), list) or not doc["assets"]:
        problems.append("the attestation carries no signed asset graph: binaries could be swapped "
                        "and SHA256SUMS recomputed without the owner signing anything")
    problems += _v3_schema_problems(doc)
    return problems


TOP_FIELDS = {"schema", "version", "public_snapshot_sha", "builder_shas", "sheets", "assets",
              "g02_manifest", "g02_manifest_sha256", "rules"}
SHEET_FIELDS = {"public_name": str, "internal_name": str, "size": int, "sha256": str,
                "blake2b_256": str, "builder_git": str, "input_blake2b256": str, "country": str,
                "layer": str, "source_release": str, "rules_blake2b_256": str}
ASSET_FIELDS = {"name": str, "size": int, "sha256": str, "blake2b_256": str, "channel": str}
CHANNELS = ("r2", "github", "both")
HEX_FIELDS = {"sha256": 64, "blake2b_256": 64, "builder_git": 40, "input_blake2b256": 64,
              "rules_blake2b_256": 64, "g02_manifest_sha256": 64, "public_snapshot_sha": 40}


def _is_exactly(value: object, typ: type) -> bool:
    """EXACT type, not `isinstance`.

    In Python `bool` is a subclass of `int`, so a JSON `true` sailed through every `size`/`count`
    check as an integer — through the FULL public CLI, on a validly signed document
.
    """
    return type(value) is typ


def _record_problems(rec: object, fields: dict, tag: str) -> list[str]:
    """Exactly the expected fields, exactly the expected types, hex where hex is meant."""
    if not isinstance(rec, dict):
        return [f"{tag}: record is not an object: {rec!r}"]
    name = rec.get("public_name") or rec.get("name") or "?"
    problems = [f"{name}: record lacks {sorted(set(fields) - set(rec))}"] if set(fields) - set(rec) else []
    extra = sorted(set(rec) - set(fields))
    if extra:
        problems.append(f"{name}: record carries unexpected field(s) {extra}")
    for field, typ in fields.items():
        if field in rec and not _is_exactly(rec[field], typ):
            problems.append(f"{name}: {field!r} is {type(rec[field]).__name__}, expected {typ.__name__}")
        elif field in HEX_FIELDS and field in rec and not _is_hex(rec[field], HEX_FIELDS[field]):
            problems.append(f"{name}: {field!r} is not {HEX_FIELDS[field]}-hex")
    if "channel" in rec and rec.get("channel") not in CHANNELS:
        problems.append(f"{name}: channel={rec.get('channel')!r} is not one of {list(CHANNELS)}")
    size = rec.get("size")
    if _is_exactly(size, int) and size < 0:
        problems.append(f"{name}: size={size} is negative — no file has a negative length, and a "
                        f"signed impossible size means the record was not derived from the bytes")
    return problems


def _sheet_asset_agreement_problems(doc: dict) -> list[str]:
    """One file, one answer: the sheet record and its asset record must carry the same triple.

    The bytes are compared against the SIGNED ASSET GRAPH once. If the sheet record were free to
    claim a different size/sha256/blake2b_256, that single physical comparison would not cover it
    and the document could contradict itself while every check still passed.
    """
    assets = {str(a.get("name")): a for a in doc.get("assets", []) if isinstance(a, dict)}
    problems = []
    for rec in doc.get("sheets", []):
        if not isinstance(rec, dict):
            continue
        asset = assets.get(str(rec.get("public_name")))
        if asset is None:
            continue                # already reported: the sheet is not covered by the asset graph
        problems += [f"{rec.get('public_name')}: the sheet record and the asset record disagree on "
                     f"{field!r} — one document, two answers about the same file"
                     for field in ("size", "sha256", "blake2b_256")
                     if rec.get(field) != asset.get(field)]
    return problems


def _rules_problems(doc: dict) -> list[str]:
    rules = doc.get("rules")
    if not isinstance(rules, dict):
        return [f"rules is {type(rules).__name__}, expected an object"]
    if set(rules) != {"count", "aggregate_blake2b_256", "files"}:
        return [f"rules has fields {sorted(rules)}, expected count/aggregate_blake2b_256/files"]
    if not _is_exactly(rules["files"], dict) or not _is_exactly(rules["count"], int):
        return ["rules fields have the wrong types"]
    if rules["count"] != len(rules["files"]):
        return [f"rules.count={rules['count']} does not match {len(rules['files'])} files"]
    if not _is_hex(rules.get("aggregate_blake2b_256"), 64):
        return ["rules.aggregate_blake2b_256 is not 64-hex"]
    return [f"rules.files[{n!r}] is not 64-hex" for n, h in sorted(rules["files"].items())
            if not _is_hex(h, 64)]


def _v3_schema_problems(doc: dict) -> list[str]:
    """The EXACT v3 schema — the same one the lab enforces.

    A shape-only check here meant two verifiers of different strictness, and then the weaker one
    defines what a release really is: a validly signed document with an extra top-level field, a
    string `size`, a string instead of `rules` and a malformed G-02 hash used to pass (
    2026-08-12).
    """
    problems = [f"attestation carries unexpected top-level field(s) {sorted(set(doc) - TOP_FIELDS)}"] \
        if set(doc) - TOP_FIELDS else []
    problems += [f"attestation has no {f!r}" for f in sorted(TOP_FIELDS - set(doc))]
    if not _is_hex(doc.get("g02_manifest_sha256"), 64):
        problems.append("g02_manifest_sha256 is not 64-hex")
    if not _is_hex(doc.get("public_snapshot_sha"), 40):
        problems.append("public_snapshot_sha is not 40-hex")
    if not isinstance(doc.get("g02_manifest"), str) or not doc.get("g02_manifest"):
        problems.append("g02_manifest is not a non-empty string")
    problems += _rules_problems(doc)

    sheets = doc.get("sheets")
    if not isinstance(sheets, list) or not sheets:
        return problems + ["attestation has no sheet records"]
    names = [r.get("public_name") for r in sheets if isinstance(r, dict)]
    if sorted(n for n in names if n) != sorted(PUBLIC_SHEETS):
        problems.append(f"sheet records {sorted(n for n in names if n)} are not exactly "
                        f"{sorted(PUBLIC_SHEETS)}: a sixth record would ride along unchecked")
    for rec in sheets:
        problems += _record_problems(rec, SHEET_FIELDS, "sheet")
    assets = doc.get("assets")
    if not isinstance(assets, list) or not assets:
        return problems + ["attestation has no signed asset graph"]
    for rec in assets:
        problems += _record_problems(rec, ASSET_FIELDS, "asset")
    names = [a.get("name") for a in assets if isinstance(a, dict)]
    if len(names) != len(set(names)):
        problems.append(f"duplicate asset records: {sorted({n for n in names if names.count(n) > 1})}")
    # Symmetry with the lab schema: every sheet must appear in the signed asset
    # graph, and an asset name must be non-empty. The public side accepted both forms.
    problems += [f"{n}: sheet is not covered by the signed asset graph"
                 for n in PUBLIC_SHEETS if n not in names]
    problems += [f"asset record with an empty name: {a!r}" for a in assets
                 if isinstance(a, dict) and not str(a.get("name", ""))]
    want = sorted({str(r.get("builder_git")) for r in sheets if isinstance(r, dict)})
    if doc.get("builder_shas") != want:
        problems.append(f"builder_shas={doc.get('builder_shas')!r} is not the recomputed {want!r}")
    return problems + _sheet_asset_agreement_problems(doc)


PROOF_FILES = (ATTESTATION_NAME, ATTESTATION_NAME + ".sig")


def manifest_problems(doc: dict, listed: dict[str, str], directory: pathlib.Path,
                      channel: str) -> list[str]:
    """`SHA256SUMS` describes the WHOLE release, and that is checked even when half is not here.

    Lines of the other half used to be skipped outright, so a truncated manifest passed: a GitHub
    set missing all five sheet lines and an  set missing the binary both returned rc=0, and
    "checksums agree" was wider than the fact. The two proof files were not compared with anything
    either — they are deliberately outside the signed graph (they cannot attest themselves), so no
    check was left for them at all.

    Now: the manifest's contents must match the signed graph plus the two proofs exactly; hashes of
    the absent half are compared with the signed records; the proofs are compared with local bytes.
    """
    signed = {str(a.get("name")): a for a in doc.get("assets", []) if isinstance(a, dict)}
    want = set(signed) | set(PROOF_FILES)
    problems = [f"{n}: signed but missing from {SUMS_NAME} — the manifest is truncated"
                for n in sorted(want - set(listed))]
    problems += [f"{n}: listed in {SUMS_NAME} but not part of the signed release"
                 for n in sorted(set(listed) - want)]
    for name in sorted(set(listed) & set(signed)):
        if listed[name] != str(signed[name].get("sha256")):
            problems.append(f"{name}: {SUMS_NAME} disagrees with the signed asset graph")
    # The proofs sit outside the signature by construction, so their only anchor is the local bytes
    # and this is the only place they can be checked. Both halves carry both files.
    for name in PROOF_FILES:
        p = directory / name
        if name in listed and p.is_file() and sha256_file(p) != listed[name]:
            problems.append(f"{name}: {SUMS_NAME} does not match the file that is right here")
    return problems


def expected_here(doc: dict, channel: str) -> set[str]:
    """The names this directory MUST carry, taken from the SIGNED graph.

    The source is the signature, not the checksum file: otherwise half of the verification could be
    switched off by editing an unsigned file.
    """
    return {str(a.get("name")) for a in doc.get("assets", [])
            if isinstance(a, dict)
            and (channel == "all" or str(a.get("channel")) in (channel, "both"))}


def asset_graph_problems(directory: pathlib.Path, doc: dict, listed: dict[str, str],
                         channel: str = "all") -> list[str]:
    """Every file shipped must be covered by the owner's signature, and SHA256SUMS must agree with
    it. Otherwise a CLI archive or wheel can be replaced and the unsigned checksum file rewritten.

    The signed graph is ONE graph but is delivered over two channels: the sheets from
    dl.gridpin.dev, the binaries from the GitHub release. A checker holding only one side declares
    it with --channel, and then the expected set is exactly that side plus the shared proof files.
    Without this, neither side could ever verify in full.
    """
    signed = {str(a.get("name")): a for a in doc.get("assets", [])
              if isinstance(a, dict)
              and (channel == "all" or str(a.get("channel")) in (channel, "both"))}
    on_disk = {p.name for p in directory.iterdir() if p.is_file() and p.name not in UNSIGNABLE}
    problems = [f"{n}: shipped but NOT covered by the owner signature"
                for n in sorted(on_disk - set(signed))]
    problems += [f"{n}: signed but not present in the download" for n in sorted(set(signed) - on_disk)]
    for name in sorted(set(signed) & on_disk):
        rec = signed[name]
        got = file_digests(directory / name)
        want = (rec.get("size"), rec.get("sha256"), rec.get("blake2b_256"))
        if got != want:
            problems.append(f"{name}: bytes do not match the SIGNED asset graph "
                            f"(size/sha256/blake2b_256 are {got} against {want})")
        if name in listed and listed[name] != str(rec.get("sha256")):
            problems.append(f"{name}: {SUMS_NAME} disagrees with the signed asset graph")
    return problems


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--dir", type=pathlib.Path, required=True)
    ap.add_argument("--allowed-signers", type=pathlib.Path, default=None)
    ap.add_argument("--expected-public-sha", required=True,
                    help="the 40-hex commit this release is being cut from; a signature stays "
                         "valid forever, so without this an older attestation would be accepted")
    ap.add_argument("--channel", choices=("all", "r2", "github"), default="all",
                    help="which half of the release this directory holds; the signed graph spans "
                         "both, so a one-sided check must say which side it has")
    ap.add_argument("--signature-only", action="store_true",
                    help="skip the on-disk asset graph (the sheets are served from dl.gridpin.dev, "
                         "not from this directory); the signature and the document are still "
                         "checked in full")
    args = ap.parse_args()
    directory = args.dir
    if not directory.is_dir():
        sys.exit(f"STOP: directory {directory} not found")
    signers = args.allowed_signers or (directory / SIGNERS_NAME)

    problems = signature_problems(directory, signers)
    if problems:
        sys.exit("STOP (release signature):\n  " + "\n  ".join(problems))
    doc, parse_bad = load_attestation(directory)
    if parse_bad:
        sys.exit("STOP (attestation):\n  " + "\n  ".join(parse_bad))
    assert doc is not None
    # The document's shape is checked in BOTH modes. It used to be checked only under
    # --signature-only, so full mode accepted a document with public_snapshot_sha="NOT-A-GIT-SHA".
    problems = shape_problems(doc, args.expected_public_sha)
    if problems:
        sys.exit("STOP (attestation shape):\n  " + "\n  ".join(problems))
    if args.signature_only:
        print("OK: owner signature verified; the attestation names this release and covers the "
              "full public sheet set")
        return
    expected = expected_here(doc, args.channel)
    sums_bad, listed = sums_problems(directory, expected)
    problems = sums_bad + manifest_problems(doc, listed, directory, args.channel) \
        + asset_graph_problems(directory, doc, listed, args.channel) \
        + attestation_problems(directory, listed, args.channel)
    if problems:
        sys.exit("STOP (release does not match its attestation):\n  " + "\n  ".join(problems))
    print(f"OK: owner signature verified; {len(listed)} assets, checksums and attestation agree")


if __name__ == "__main__":
    main()
