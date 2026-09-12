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

## Germany — 15 of 16 Länder; Bavaria not covered

The address base is Overture Maps Addresses **2026-08-19.0**, reached through
OpenAddresses. The following per-Land catalog is retained in the sheet's
`source_catalog`; attribution records were accessed 2026-08-20. Data was
filtered to the declared coverage, normalized and indexed by GridPin. These
licenses are per source, not a new blanket license for all German addresses.

| Land | Provider / attribution | Source | License |
|---|---|---|---|
| Brandenburg | Landesvermessung und Geobasisinformation Brandenburg (LGB) | [Georeferenzierte Adresse](https://geobasis-bb.de/lgb/de/geodaten/liegenschaftskataster/georeferenzierte-adresse/) | [Datenlizenz Deutschland – Namensnennung – Version 2.0](https://www.govdata.de/dl-de/by-2-0) |
| Berlin | Geoportal Berlin / Land Berlin | [Georeferenzierte Gebäudeadressen Berlin](https://gdi.berlin.de/geonetwork/srv/ger/catalog.search) | [Datenlizenz Deutschland – Zero – Version 2.0](https://www.govdata.de/dl-de/zero-2-0) |
| Baden-Württemberg | Landesamt für Geoinformation und Landentwicklung Baden-Württemberg (LGL) | [Hauskoordinaten](https://www.lgl-bw.de/Produkte/Liegenschaftskataster/Hauskoordinaten/) | [Datenlizenz Deutschland – Namensnennung – Version 2.0](https://www.govdata.de/dl-de/by-2-0) |
| Bremen | GeoInformation Bremen | [ALKIS Hauskoordinaten](https://www.geo.bremen.de/produkte/katasterprodukte/auszuege-aus-dem-liegenschaftskataster-12272) | [Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/legalcode.en) |
| Hessen | Hessisches Landesamt für Bodenmanagement und Geoinformation (HLBG) | [Hauskoordinaten ohne postalische Angaben](https://gds.hessen.de/INTERSHOP/web/WFS/HLBG-Geodaten-Site/de_DE/-/EUR/ViewDownloadcenter-Start?path=Liegenschaftskataster%2FHauskoordinaten+ohne+Postalische+Angaben+%28txt%29) | [Datenlizenz Deutschland – Zero – Version 2.0](https://www.govdata.de/dl-de/zero-2-0) |
| Hamburg | Landesbetrieb Geoinformation und Vermessung Hamburg (LGV) | [ALKIS Adressen Hamburg](https://suche.transparenz.hamburg.de/dataset/alkis-adressen-hamburg6) | [Datenlizenz Deutschland – Namensnennung – Version 2.0](https://www.govdata.de/dl-de/by-2-0) |
| Mecklenburg-Vorpommern | Landesamt für innere Verwaltung Mecklenburg-Vorpommern (LAiV) | [ALKIS Adressen](https://laiv.geodaten-mv.de/afgvk/Liegenschaftskataster/Beschreibung?produkt=ALKIS) | [Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/legalcode.en) |
| Niedersachsen | Landesamt für Geoinformation und Landesvermessung Niedersachsen (LGLN) | [Liegenschaftskataster Hauskoordinaten](https://ni-lgln-opengeodata.hub.arcgis.com/search?tags=liegenschaftskataster) | [Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/legalcode.en) |
| Nordrhein-Westfalen | Bezirksregierung Köln, Geobasis NRW | [Georeferenzierte Gebäudeadressen (gebref_txt)](https://www.opengeodata.nrw.de/produkte/geobasis/lk/akt/gebref_txt/) | [Datenlizenz Deutschland – Zero – Version 2.0](https://www.govdata.de/dl-de/zero-2-0) |
| Rheinland-Pfalz | Landesamt für Vermessung und Geobasisinformation Rheinland-Pfalz (LVermGeo) | [Hauskoordinaten](https://lvermgeo.rlp.de/geodaten-geoshop/open-data) | [Datenlizenz Deutschland – Namensnennung – Version 2.0](https://www.govdata.de/dl-de/by-2-0) |
| Schleswig-Holstein | Landesamt für Vermessung und Geoinformation Schleswig-Holstein (LVermGeo SH) | [Liegenschaftskataster Hauskoordinaten](https://geodaten.schleswig-holstein.de/gaialight-sh/_apps/dladownload/lizenz.html) | [Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/legalcode.en) |
| Saarland | Landesamt für Vermessung, Geoinformation und Landentwicklung Saarland (LVGL) | [Hauskoordinaten](https://geoportal.saarland.de/) | [Datenlizenz Deutschland – Namensnennung – Version 2.0](https://www.govdata.de/dl-de/by-2-0) |
| Sachsen | Staatsbetrieb Geobasisinformation und Vermessung Sachsen (GeoSN) | [Hauskoordinaten](https://www.geodaten.sachsen.de/downloadbereich-hauskoordinaten-4172.html) | [Datenlizenz Deutschland – Namensnennung – Version 2.0](https://www.govdata.de/dl-de/by-2-0) |
| Sachsen-Anhalt | Landesamt für Vermessung und Geoinformation Sachsen-Anhalt (LVermGeo) | [Hauskoordinaten](https://www.lvermgeo.sachsen-anhalt.de/de/gdp-open-data.html) | [Datenlizenz Deutschland – Namensnennung – Version 2.0](https://www.govdata.de/dl-de/by-2-0) |
| Thüringen | Thüringer Landesamt für Bodenmanagement und Geoinformation (TLBG) | [Hauskoordinaten](https://geoportal.thueringen.de/gdi-th/download-offene-geodaten) | [Datenlizenz Deutschland – Namensnennung – Version 2.0](https://www.govdata.de/dl-de/by-2-0) |

Additional postcode witness: **Bundesnetzagentur**, public charging-station
register, snapshot **2026-07-28**, **CC BY 4.0**. Attribution:
Source: [retained register](https://data.bundesnetzagentur.de/Bundesnetzagentur/DE/Fachthemen/ElektrizitaetundGas/E-Mobilitaet/Ladesaeulenregister_BNetzA_2026-07-28.csv);
[license](https://creativecommons.org/licenses/by/4.0/deed.de).
© Bundesnetzagentur.de. Only 14,528 previously blank postcode rows were filled
across 13,870 existing address identities; **zero address rows were added**.
All 19,267,049 base rows and coordinates were preserved. This is not a charging
station POI layer. The original source metadata and indication of changes remain
inside the release sheet.
