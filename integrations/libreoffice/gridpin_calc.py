"""GridPin for LibreOffice Calc: geocode the selected column of addresses with a GridPin server on this machine.

Tools > Macros > Run Macro > My Macros > gridpin_calc > geocode_selection. The four columns to the right of the
selection get gp_lat, gp_lon, gp_label and gp_weak. Answers are kept in a hidden sheet of the document (gridpin_cache),
so a second run sends only the addresses it has not seen. Addresses travel in the body of a POST to
http://127.0.0.1:8080 (set GRIDPIN_SERVER to change it); nothing goes to a cloud service. Standard library only: it
runs in the Python that comes with LibreOffice.
"""
import json
import os
import urllib.error
import urllib.request

CACHE = "gridpin_cache"
COLUMNS = ("gp_lat", "gp_lon", "gp_label", "gp_weak")
BATCH = 1000


def server():
    return os.environ.get("GRIDPIN_SERVER", "http://127.0.0.1:8080").rstrip("/")


def ask(addresses, base=None):
    """One answer (a hit dict, or None) per address, asking the server BATCH addresses per request."""
    base = base or server()
    out = []
    for i in range(0, len(addresses), BATCH):
        body = json.dumps({"queries": addresses[i:i + BATCH], "k": 1}).encode("utf-8")
        req = urllib.request.Request(base + "/v1/geocode", data=body, headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=300) as r:
                results = json.load(r)["results"]
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"#GRIDPIN: server answered {e.code}") from e
        except (urllib.error.URLError, OSError) as e:
            raise RuntimeError("#GRIDPIN: server not running at " + base) from e
        out += [r[0] if r else None for r in results]
    return out


def label(hit):
    street = " ".join(p for p in (hit.get("housenumber") or "", hit.get("street") or "") if p)
    return ", ".join(p for p in (street, hit.get("commune") or "") if p)


def _cache_sheet(doc):
    sheets = doc.getSheets()
    if not sheets.hasByName(CACHE):
        sheets.insertNewByName(CACHE, sheets.getCount())
        sheets.getByName(CACHE).IsVisible = False
    return sheets.getByName(CACHE)


def _read_cache(sheet):
    found, row = {}, 0
    while True:
        key = sheet.getCellByPosition(0, row).getString()
        if not key:
            return found, row
        found[key] = json.loads(sheet.getCellByPosition(1, row).getString() or "null")
        row += 1


def geocode_range(doc, sheet, column, first_row, last_row, base=None):
    """Geocode the addresses in `column`, rows first_row..last_row (0-based) of `sheet`; returns how many were sent."""
    cache_sheet = _cache_sheet(doc)
    cache, free_row = _read_cache(cache_sheet)
    addresses = [sheet.getCellByPosition(column, r).getString().strip() for r in range(first_row, last_row + 1)]
    todo = sorted({a for a in addresses if a and a not in cache})
    try:
        answers = ask(todo, base) if todo else []
    except RuntimeError as e:
        sheet.getCellByPosition(column + 1, first_row).setString(str(e))
        return 0
    for address, hit in zip(todo, answers):
        cache[address] = hit
        cache_sheet.getCellByPosition(0, free_row).setString(address)
        cache_sheet.getCellByPosition(1, free_row).setString(json.dumps(hit))
        free_row += 1
    for offset, address in enumerate(addresses):
        row = first_row + offset
        hit = cache.get(address) if address else None
        cells = [sheet.getCellByPosition(column + 1 + i, row) for i in range(4)]
        if hit is None:
            for c in cells:
                c.setString("")
            continue
        cells[0].setValue(hit["lat"])
        cells[1].setValue(hit["lon"])
        cells[2].setString(label(hit))
        cells[3].setString("weak" if hit.get("weak") else "")
    return len(todo)


def geocode_selection(*_args):
    """The macro: the selection is one column of addresses; a first cell named like a header is left alone."""
    doc = XSCRIPTCONTEXT.getDocument()  # noqa: F821 - provided by LibreOffice to macros
    rng = doc.getCurrentController().getSelection().getRangeAddress()
    sheet = doc.getSheets().getByIndex(rng.Sheet)
    first = rng.StartRow
    if sheet.getCellByPosition(rng.StartColumn, first).getString().strip().lower() in ("address", "adresse", "addr"):
        for i, name in enumerate(COLUMNS):
            sheet.getCellByPosition(rng.StartColumn + 1 + i, first).setString(name)
        first += 1
    geocode_range(doc, sheet, rng.StartColumn, first, rng.EndRow)


g_exportedScripts = (geocode_selection,)
