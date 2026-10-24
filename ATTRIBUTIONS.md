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
| CartoCiudad — Instituto Geográfico Nacional / CNIG, scne.es (Spain), via Overture/OpenAddresses (release 2026-08-19.0) | Spain addresses (all 52 postal provinces, incl. Ceuta and Melilla) | **CC BY 4.0** | Obra derivada de CartoCiudad CC-BY 4.0 scne.es — address normalization applied |
| IBGE — Instituto Brasileiro de Geografia e Estatistica, CNEFE 2022; via Overture Maps / OpenAddresses, release 2026-08-19.0 | Brazil addresses (nationwide) | Public data; implied licence recorded by AddressForAll as CC0 family; labelled CC0 by Overture | Address normalization applied |
| National Address Database (USDOT) and 19 state and county open datasets (New York City, Los Angeles, Oakland County, MassGIS, MnGeo, Oregon and others), via Overture Maps / OpenAddresses, release 2026-09-23.1; address ranges from U.S. Census Bureau TIGER/Line 2025 ADDRFEAT; postal codes from GeoNames | United States addresses (50 states and DC; point addresses in 35 states, address-range interpolation elsewhere) | **mixed permissive**: U.S. Government work (17 U.S.C. §105), NYC Open Data (Admin Code §23-502), CC0 1.0, PDDL 1.0, CC BY 4.0 and county open-data terms — per source as listed in the sheet manifest; TIGER/Line is a U.S. Government work; GeoNames postal codes CC BY 4.0 | National Address Database (USDOT) and state and county open data via Overture Maps / OpenAddresses; U.S. Census Bureau TIGER/Line 2025; GeoNames (CC BY 4.0) |
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

## United States (us.bin) — partial coverage; only sources with a direct right to redistribute

The address base is Overture Maps Addresses **2026-09-23.1**, reached through
OpenAddresses and the federal National Address Database (NAD, version `20250924`
as carried in Overture). The sheet carries only the **20 datasets** whose publishers
grant redistribution by licence or by statute (licence check of the primary
sources, 2026-10-19; Overture [attribution page](https://docs.overturemaps.org/attribution/),
United States section, accessed the same day). County data distributed under
public-records case law (California, Florida), Colorado, the Michigan county
licences and Mississippi are **not** in the sheet, so coverage is partial by state.
Data was filtered to that subset, normalized and indexed by GridPin —
**address normalization applied** to every source below. These licences are per
source, not a new blanket licence for all United States addresses. Dataset names
are Overture `sources[].dataset` values; where a publisher's own terms are not
reproduced here, see the OpenAddresses source descriptor
`openaddresses/openaddresses/sources/<dataset>.json`.

| Dataset (Overture) | Publisher / attribution | Source | License | Required notice |
|---|---|---|---|---|
| `NAD` (43 states, DC, VI) | U.S. Department of Transportation (USDOT) and the contributing state, local and tribal governments — National Address Database (NAD), version 20250924 | [National Address Database](https://www.transportation.gov/gis/national-address-database) | Work of the U.S. federal government, not subject to copyright under 17 U.S.C. § 105; "available for re-use without limitation or restriction" — [NAD Disclaimer](https://www.transportation.gov/mission/open/gis/national-address-database/national-address-database-nad-disclaimer) | **Not intended for use as a mailing list** and subject to individual state statutes that prohibit its use as a mailing list. Provided "as is". Source: USDOT National Address Database |
| `us/ny/city_of_new_york` | NYC Department of City Planning, via NYC Open Data | [AddressPoint, NYC Open Data](https://data.cityofnewyork.us/City-Government/AddressPoint/uf93-f8nk) | NYC Administrative Code § 23-502(d): public data sets are made available without registration requirement, license requirement or restrictions on their use; [Open Data Terms of Use for NYC Open Data](https://opendata.cityofnewyork.us/overview/) | Source: NYC Open Data, AddressPoint (NYC Department of City Planning); version: as carried in Overture Maps release 2026-09-23.1 via OpenAddresses `us/ny/city_of_new_york`; modifications: address normalization applied |
| `us/ca/fresno` | Fresno County | [Fresno County data sharing](https://datasharing-cofgisonline.hub.arcgis.com/pages/our-data) | [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/) as recorded on the Overture attribution page; the OpenAddresses descriptor links to the county's public-records page | None required; credit given: Fresno County |
| `us/la/city_of_new_orleans` | City of New Orleans | [Site Address Point, data.nola.gov](https://data.nola.gov/Public-Safety-and-Preparedness/Site-Address-Point/hfvu-md72) | [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/) | None required; credit given: City of New Orleans |
| `us/ca/san_francisco` | City and County of San Francisco | [Addresses with Units — Enterprise Addressing System, DataSF](https://data.sfgov.org/Geographic-Locations-and-Boundaries/Addresses-with-Units-Enterprise-Addressing-System/ramy-di5m) | [Public Domain Dedication and License (PDDL) v1.0](https://opendatacommons.org/licenses/pddl/1-0/) | None required; credit given: City and County of San Francisco |
| `us/pa/lackawanna` | City of Scranton GIS | [Scranton Open Data](https://scranton-open-data-scrantonplanning.hub.arcgis.com/datasets/6eb6dbd15aef44a0a2c2b388d893f906/about) | [PDDL 1.0](https://opendatacommons.org/licenses/pddl/summary/) | None required; credit given: City of Scranton GIS |
| `us/ca/napa` | Napa County | [Addresses (public), Napa County GIS](https://gisdata.napacounty.gov/datasets/napacounty::addresses-public-1/about) | [Public Domain Dedication and License (PDDL) v1.0](https://opendatacommons.org/licenses/pddl/1-0/) | None required; credit given: Napa County |
| `us/fl/hillsborough` | Hillsborough County | [Site Address Points](https://gis2017-01-10t133755357z-hillsborough.opendata.arcgis.com/datasets/hillsborough::site-address-points/about) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) | © Hillsborough County, CC BY 4.0 — address normalization applied |
| `us/fl/manatee` | Manatee County | [Address Points](https://public-manateegis.opendata.arcgis.com/datasets/manateegis::address-points-1/about) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) | © Manatee County, CC BY 4.0 — address normalization applied |
| `us/ga/dekalb` | DeKalb County | [Address](https://dekalbinsights-dekalbgis.opendata.arcgis.com/datasets/DeKalbGIS::address-6/about) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) | © DeKalb County, CC BY 4.0 — address normalization applied |
| `us/ga/clarke` | Athens-Clarke County GIS | [ACC Address Point](https://data-athensclarke.opendata.arcgis.com/datasets/AthensClarke::acc-address-point/about) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) | © Athens-Clarke County GIS, CC BY 4.0 — address normalization applied |
| `us/id/ada` | City of Boise | [City of Boise Open Data](https://opendata.cityofboise.org/) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) | © City of Boise, CC BY 4.0 — address normalization applied |
| `us/wi/waupaca` | Waupaca County, Wisconsin | [Addresses](https://data2017-04-05t135915451z-waupacacounty.opendata.arcgis.com/datasets/waupacacounty::addresses/about) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) | © Waupaca County, Wisconsin, CC BY 4.0 — address normalization applied |
| `us/ca/nevada` | County of Nevada, California (Nevada County) | [Parcel Situs Address](https://maps-nevcounty.opendata.arcgis.com/datasets/nevcounty::parcel-situs-address/about) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) | © County of Nevada, California, CC BY 4.0 — address normalization applied |
| `us/ca/sacramento` | Sacramento County | [Address](https://data-sacramentocounty.opendata.arcgis.com/datasets/sacramentocounty::address-1/about) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) | © Sacramento County, CC BY 4.0 — address normalization applied |
| `us/ca/los_angeles` | County of Los Angeles (Los Angeles County) | [CAMS export data, LA County Open Data](https://data.lacounty.gov/datasets/cams-export-data/about) | [County of Los Angeles Open Data Terms of Use](https://lacounty.maps.arcgis.com/sharing/rest/content/items/fa47b95a2f704df094cc19b834f4fb07/data), last updated 2015-04-15, § 3: licence to copy, publish, distribute and/or transmit the Data, to adapt the Data and to exploit the Data for commercial and/or personal use (the Overture attribution page records "CC0"; the County's own terms govern) | Source: Los Angeles County. Nothing in GridPin suggests the County's endorsement of this use |
| `us/mi/oakland` | Oakland County, Michigan | [OC Site Address, Access Oakland](https://accessoakland.oakgov.com/datasets/oakgov::oc-site-address/about) | Oakland County Open Data Terms of Use Agreement (on the dataset page), § 3: copy, modify, translate, adapt, distribute, sort, search and reuse Open Data for any lawful purpose | Source: Oakland County. No endorsement by the County is implied |
| `us/ma/statewide` | MassGIS (Bureau of Geographic Information), Commonwealth of Massachusetts EOTSS | [MassGIS Master Address Points](https://gis.data.mass.gov/maps/a0049ad319674c439516b2df1173d8f5/about) | Public records; the service "may be used by anyone for their purposes" (MassGIS dataset terms, read 2026-10-19); recorded as Public Domain on the Overture attribution page | Credit requested by the publisher: "MassGIS (Bureau of Geographic Information), Commonwealth of Massachusetts EOTSS" |
| `us/mn/statewide` | Minnesota Geospatial Information Office (MnGeo) — "Address Points, Compiled from Opt-In Open Counties, Minnesota" (the Overture attribution page names the Department of Public Safety; the dataset metadata names MnGeo) | [Address Points, Compiled from Opt-In Open Counties](https://gisdata.mn.gov/dataset/loc-addresses-open) | Dataset metadata (2026-10-08): Access Constraints: None; Use Constraints: None | None required; credit given: MnGeo |
| `us/or/statewide` | Oregon GEOHub, State of Oregon; compiled from Oregon 9-1-1 Public Safety Answering Points (PSAP), Oregon Department of Emergency Management (OEM), Oregon Department of Administrative Services (DAS) | [Oregon Address Points](https://geohub.oregon.gov/datasets/oregon-geo::oregon-address-points/about) | Publicly shared under ORS 276A.509; [GEOHub Terms of Use](https://geohub.oregon.gov/pages/terms-of-use) v1.0 (2023-05-14): written agreements for public data are not required, use including for any commercial purpose | None required ("credits and caveats are included in the metadata"); credits given here |

In the Overture data the rows of `us/ca/napa`, `us/ca/nevada` and `us/ca/sacramento`
carry the code `LicenseRef-Public-Domain-US-CA-Law`; the publishers' own licences
(PDDL 1.0 / CC BY 4.0) recorded above are the ones honoured. The standard licences
(CC0, PDDL, CC BY) were taken from the Overture attribution page and the
OpenAddresses descriptors; they were not re-checked on the publishers' own sites.

### Interpolation layer — TIGER/Line address ranges (outside the ZIPs covered above)

| Source | What it supplies | Licence | Attribution / notes |
|---|---|---|---|
| U.S. Census Bureau — TIGER/Line Shapefiles, Address Range-Feature (ADDRFEAT), vintage 2025 | house-number ranges per road edge, turned into interpolated *range points* (answer precision `interp`, never `house`) for ZIPs with no point source above | Work of the U.S. federal government, not subject to copyright (17 U.S.C. § 105); [TIGER/Line Shapefiles](https://www.census.gov/geographies/mapping-files/time-series/geo/tiger-line-file.html) are released without restriction | Source: U.S. Census Bureau, TIGER/Line Shapefiles 2025. Ranges are theoretical ("potential") address ranges, not observed addresses; positions are interpolated along the road centreline |
| GeoNames postal codes (US) | USPS place name of each ZIP, used as the city of a range point | CC BY 4.0 | © GeoNames, geonames.org (CC BY 4.0) |
