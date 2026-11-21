"""Polish voivodeships: the GeoNames admin1 code -> the code the sheet carries (ISO 3166-2 PL-XX without "PL-").

Overture's Polish addresses name the voivodeship by its lower-case Polish name (`address_levels[1]`: "mazowieckie",
"dolnośląskie"); OpenAddresses files them under ISO 3166-2 codes (PL-MZ, PL-LD). GeoNames numbers them 72-87 in its
own order. The map was checked on 2026-11-13 against the capitals in the PL dump (PPLC/PPLA): 72 Wrocław, 73 Bydgoszcz,
74 Łódź, 75 Lublin, 76 Gorzów Wielkopolski and Zielona Góra, 77 Kraków, 78 Warsaw, 79 Opole, 80 Rzeszów, 81 Białystok,
82 Gdańsk, 83 Katowice, 84 Kielce, 85 Olsztyn, 86 Poznań, 87 Szczecin.
"""

GEONAMES_ADMIN1_TO_VOIVODESHIP = {
    "72": "DS", "73": "KP", "74": "LD", "75": "LU", "76": "LB", "77": "MA", "78": "MZ", "79": "OP",
    "80": "PK", "81": "PD", "82": "PM", "83": "SL", "84": "SK", "85": "WN", "86": "WP", "87": "ZP",
}

# The voivodeship as Overture writes it (address_levels[1]) -> the sheet code.
OVERTURE_NAME_TO_VOIVODESHIP = {
    "dolnośląskie": "DS", "kujawsko-pomorskie": "KP", "łódzkie": "LD", "lubelskie": "LU", "lubuskie": "LB",
    "małopolskie": "MA", "mazowieckie": "MZ", "opolskie": "OP", "podkarpackie": "PK", "podlaskie": "PD",
    "pomorskie": "PM", "śląskie": "SL", "świętokrzyskie": "SK", "warmińsko-mazurskie": "WN", "wielkopolskie": "WP",
    "zachodniopomorskie": "ZP",
}
