"""Canadian province codes: GeoNames admin1 (two digits) -> the two-letter code the sheet carries.

Overture's Canadian addresses name the province by its postal abbreviation (`address_levels[1]`:
ON, QC, ...), while the GeoNames CA dump uses FIPS-style admin1 numbers. The map was checked on
2026-10-28 against the capitals of the dump (PPLC/PPLA): 01 Edmonton, 02 Victoria, 03 Winnipeg,
04 Fredericton, 05 St. John's, 07 Halifax, 08 Ottawa and Toronto, 09 Charlottetown, 10 Québec,
11 Regina, 12 Whitehorse, 13 Yellowknife, 14 Iqaluit. Code 06 is unused (Northwest Territories
before the 1999 split).
"""

GEONAMES_ADMIN1_TO_PROVINCE = {
    "01": "AB", "02": "BC", "03": "MB", "04": "NB", "05": "NL", "07": "NS", "08": "ON",
    "09": "PE", "10": "QC", "11": "SK", "12": "YT", "13": "NT", "14": "NU",
}
