# GridPin in Raycast and Alfred

Type or copy an address, get its point from a local GridPin file: offline, no API key.

## Raycast

Raycast > Settings > Extensions > Script Commands > Add Directories: the folder of `gridpin-locate.sh`. Set
`GRIDPIN_SHEET` (the path of a country `.bin` file) in the environment Raycast starts scripts with, or edit the line
`sheet=` of the script. The command copies `lat, lon` to the clipboard and shows:

```text
43.7436618, 7.428483 — 27 Boulevard des Moulins, Monaco [house]
https://www.openstreetmap.org/?mlat=43.7436618&mlon=7.428483#map=18/43.7436618/7.428483
```

An answer that needs a check says so (`— check it (weak answer)`). With no argument the clipboard is read.

## Alfred

Workflows > + > Blank Workflow; add Inputs > Script Filter (bash, "with input as argv") running `gridpin-alfred.sh`,
connect it to Outputs > Copy to Clipboard, and set the workflow variable `GRIDPIN_SHEET`. Five answers per address;
Enter copies `lat, lon`, Cmd+Enter opens the map.

Both need the `gridpin` command on PATH (or `GRIDPIN_BIN`). The same scripts work from a terminal.
