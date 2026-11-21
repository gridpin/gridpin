"""New Zealand regions: the GeoNames admin1 code -> the ISO 3166-2:NZ code the reference carries.

Overture's New Zealand addresses (LINZ NZ Addresses via OpenAddresses) carry no region at all: two levels, the town or
city and the suburb or locality. The region codes serve only the commune_regions table of the rule pack (where a
place name lies) and a later quality cycle; the first sheet reads no region in the query (ANSWERS §364, plan §5).
The map was checked on 2026-11-17 against the PPLA/PPLC rows of the NZ dump: 10 Waitangi (Chatham Islands),
E7 Auckland, E8 Whakatane, E9 Christchurch, F1 Gisborne, F2 Napier, F3 Palmerston North, F4 Blenheim, F5 Nelson,
F6 Whangarei, F7 Dunedin, F8 Invercargill, F9 New Plymouth, G1 Hamilton, G2 Wellington, G3 Greymouth, TAS Richmond.
"""

GEONAMES_ADMIN1_TO_REGION = {
    "10": "CIT", "E7": "AUK", "E8": "BOP", "E9": "CAN", "F1": "GIS", "F2": "HKB", "F3": "MWT", "F4": "MBH",
    "F5": "NSN", "F6": "NTL", "F7": "OTA", "F8": "STL", "F9": "TKI", "G1": "WKO", "G2": "WGN", "G3": "WTC",
    "TAS": "TAS",
}
