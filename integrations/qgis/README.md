# GridPin Geocoder for QGIS

Offline geocoding in QGIS: turn an address field into points, or jump to an address from the
locator bar. Queries never leave your machine; the only network use is downloading a country
sheet once from `dl.gridpin.dev`, checked against the signed release (size and SHA-256).

Coverage is limited to the countries listed at https://gridpin.dev.

## Install

1. Plugins > Manage and Install Plugins > search "GridPin Geocoder" > Install. QGIS also offers
   the QPIP plugin, which installs the `gridpin` Python package from `requirements.txt`.
2. Processing Toolbox > GridPin > **Download country sheet**, pick the country.

Without QPIP, install the engine into the QGIS Python yourself:
`python3 -m pip install "gridpin>=0.5,<0.6"`.

## Use

- **Processing > GridPin > Geocode address field**: any layer or table, one address field,
  one country. The output is a point layer in EPSG:4326 with every input row; rows without a
  match have no geometry. Added fields: `gp_address`, `gp_precision`, `gp_confidence`,
  `gp_lat`, `gp_lon`. Works in the graphical modeler and from `processing.run("gridpin:geocode", ...)`.
- **Locator bar**: type `gp 1600 pennsylvania ave washington` and pick a result; the map zooms
  to it. Every downloaded sheet is searched.

Sheets live in `GRIDPIN_HOME`, or `~/.cache/gridpin` by default, shared with the other GridPin
tools on the machine.

## Development

The plugin package is `gridpin_geocoder/` (QGIS 3.44 LTR and 4.x, Qt5 and Qt6). Tests run inside
the official QGIS images against the Monaco smoke sheet (`make mc` first):

    bash integrations/qgis/tests/run_in_docker.sh

## License

The plugin is GPL-2.0-or-later (`gridpin_geocoder/LICENSE`), as the QGIS plugin repository
requires. The `gridpin` engine it calls is Apache-2.0. Sheet data: see https://gridpin.dev.
