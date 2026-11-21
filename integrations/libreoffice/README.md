# GridPin in LibreOffice Calc

A macro that geocodes the selected column of addresses with a GridPin server on your own machine: offline, no API key.

1. Start the server: `gridpin serve france.bin` (it listens on `http://127.0.0.1:8080`; set `GRIDPIN_SERVER` for
   another address).
2. Copy `gridpin_calc.py` into your LibreOffice user scripts folder, `Scripts/python/` of the profile
   (macOS: `~/Library/Application Support/LibreOffice/4/user/Scripts/python/`, Linux: `~/.config/libreoffice/4/user/Scripts/python/`,
   Windows: `%APPDATA%\LibreOffice\4\user\Scripts\python\`).
3. Select the column of addresses (a first cell named `address` is read as a header) and run Tools > Macros >
   Run Macro > My Macros > gridpin_calc > geocode_selection.

The four columns to the right get `gp_lat`, `gp_lon`, `gp_label` and `gp_weak` (`weak`: check that answer before use).
A row without an address or without an answer stays empty. Answers are kept in a hidden sheet of the document,
`gridpin_cache`: running the macro again sends only addresses it has not seen. If the server is not running, the first
cell says so: `#GRIDPIN: server not running at http://127.0.0.1:8080`.

Addresses travel in the body of a POST to the local server, 1,000 per request. Standard library only: the macro runs
in the Python that comes with LibreOffice.

Why a macro and not a cell function `=GRIDPIN(A2)`: a Calc add-in function needs a compiled UNO type library, made with
the LibreOffice SDK; the macro needs nothing beyond LibreOffice.

## Tests

`make test-libreoffice` runs the macro inside a real headless LibreOffice (a throwaway profile, `gridpin serve` on the
Monaco smoke sheet). Checked with LibreOffice 26.2 on macOS.
