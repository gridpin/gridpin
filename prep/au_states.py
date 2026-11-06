"""Australian states and territories: the GeoNames admin1 code -> the code the sheet carries.

Overture's Australian addresses already name the state by its code (`address_levels[1]`: NSW, VIC, ...), including
OT for the other territories (Norfolk Island, Christmas Island, the Cocos Islands, Jervis Bay). GeoNames numbers the
eight states in its own order and files Norfolk, Christmas and Cocos under countries of their own, so OT has no
admin1 code. The map was checked on 2026-11-04 against the capitals of the AU dump (PPLC/PPLA): 01 Canberra, 02 Sydney,
03 Darwin, 04 Brisbane, 05 Adelaide, 06 Hobart, 07 Melbourne, 08 Perth.
"""

GEONAMES_ADMIN1_TO_STATE = {
    "01": "ACT", "02": "NSW", "03": "NT", "04": "QLD", "05": "SA", "06": "TAS", "07": "VIC", "08": "WA",
}
