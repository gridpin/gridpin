# Data attributions

GridPin builds derived indexes from open data. The data belongs to its
publishers and is redistributed on their terms — one address-data lineage per
sheet. Auxiliary layers are listed below (e.g. Serbia additionally carries
GeoNames-derived city aliases, CC BY 4.0).

Addresses are NOT one blanket license — each national source carries its own, per the
[Overture attribution page](https://docs.overturemaps.org/attribution/) (verified 2026-07).
Addresses reach Overture via OpenAddresses.

| Source | Used for | License | Attribution |
|---|---|---|---|
| Base Adresse Nationale (BAN), France — adresse.data.gouv.fr | France addresses | Licence Ouverte / Open Licence 2.0 (Etalab 2.0) | Base Adresse Nationale — Etalab 2.0 |
| ANNCSU (Italy), via Overture/OpenAddresses | Italy addresses | **CC BY 4.0** | Archivio Nazionale dei Numeri Civici delle Strade Urbane (ANNCSU) — CC BY 4.0 |
| Nationaal Georegister / BAG (Netherlands), via Overture/OpenAddresses | Netherlands addresses | **Public Domain Mark 1.0** (no rights reserved) | Nationaal Georegister (Kadaster / BAG) — PDM 1.0 |
| Republički geodetski zavod (RGZ), data.gov.rs (Serbia), via Overture/OpenAddresses | Serbia addresses | **data.gov.rs Terms of use** | Републички геодетски завод (RGZ) — data.gov.rs Terms of use |
| Overture Maps Foundation, places layer | optional POI sheet | **mixed permissive**: CDLA-Permissive-2.0 (Meta, Microsoft, …), Apache-2.0 (Foursquare), CC0-1.0 (AllThePlaces) | © Overture Maps Foundation and per-source contributors |
| GeoNames | settlement-name aliases (multi-script, Serbia) | CC BY 4.0 | © GeoNames, geonames.org (CC BY 4.0) |
| Who's on First | admin polygons for reverse lookup (where used) | CC0 core (some geometries CC BY) | © Who's on First / contributors |
| OpenStreetMap (Geofabrik extracts) | **Smoke test only**: a small pinned Monaco extract, `eval/smoke/fixtures/monaco.osm.pbf`, is shipped in this repository so `make mc` builds a tiny Monaco index reproducibly from fixed bytes. This is the only OSM-derived file distributed; no sold country sheet contains OSM data, and OSM is not used to train the distributed models | ODbL 1.0 (applies to the shipped `.pbf` fixture and any index built from it) | © OpenStreetMap contributors, openstreetmap.org/copyright |

The parsing and ranking models shipped in `ml/` are trained only on synthetic
strings generated from the permissively licensed corpora above (BAN and Overture:
France, Netherlands, Italy, Serbia). No copyleft (ODbL/OSM) data enters the
distributed model weights.
