# tiny.bin

A 1 KB GridPin sheet with six synthetic addresses: Main Street in two made-up towns (Springfield,
Shelbyville) and Oak Avenue in Springfield (country code `zz`), for examples and tests without a
download. Built with the GridPin engine:

    gridpin build tiny.csv tiny.bin --meta tiny.json

from the rows in `tiny.csv` next to this file, sorted by street and town (the builder requires it).
The rows are invented; Apache-2.0 like the package.
