"""Runs inside LibreOffice (a user script of a throwaway profile; test/run.py puts it there). Fills a Calc document,
runs the macro's geocode_range twice (the second run must send nothing: the answers are in the document), once more
against a server that is down, writes what it saw to $GRIDPIN_TEST_OUT and closes LibreOffice."""
import json
import os

import gridpin_calc


def run_check(*_args):
    desktop = XSCRIPTCONTEXT.getDesktop()  # noqa: F821 - provided by LibreOffice
    out = {}
    try:
        doc = desktop.loadComponentFromURL("private:factory/scalc", "_blank", 0, ())
        sheet = doc.getSheets().getByIndex(0)
        rows = ["address", "boulevard des moulins 27 monaco", "", "zzzz qqqq", "monaco", "boulevard des moulins 27 monaco"]
        for r, text in enumerate(rows):
            sheet.getCellByPosition(0, r).setString(text)
        base = os.environ["GRIDPIN_SERVER"]
        out["sent_first"] = gridpin_calc.geocode_range(doc, sheet, 0, 1, 5, base)
        out["rows"] = [[sheet.getCellByPosition(c, r).getString() for c in range(1, 5)] for r in range(1, 6)]
        out["lat_value"] = sheet.getCellByPosition(1, 1).getValue()
        out["sent_second"] = gridpin_calc.geocode_range(doc, sheet, 0, 1, 5, base)
        sheet.getCellByPosition(0, 6).setString("avenue de la costa 1 monaco")
        gridpin_calc.geocode_range(doc, sheet, 0, 6, 6, "http://127.0.0.1:1")
        out["server_down"] = sheet.getCellByPosition(1, 6).getString()
        out["cache_hidden"] = not doc.getSheets().getByName(gridpin_calc.CACHE).IsVisible
        doc.close(True)
    except Exception as e:  # noqa: BLE001 - reported to the runner
        out["error"] = repr(e)
    with open(os.environ["GRIDPIN_TEST_OUT"], "w") as f:
        json.dump(out, f, ensure_ascii=False)
    desktop.terminate()


g_exportedScripts = (run_check,)
