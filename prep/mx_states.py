"""Mexican states: the GeoNames admin1 code and the Overture state name -> the three-letter code the sheet carries.

The sheet keys a state by its ISO 3166-2:MX code without the country prefix (JAL, CMX, NLE, ...), as the US and
Canadian sheets key theirs by a two-letter code. Overture's Mexican addresses name the state in full
(`address_levels[1]`: "Veracruz de Ignacio de la Llave", "Ciudad de México", "México" -- the State of Mexico).
GeoNames numbers the states in its own order, which is NOT the INEGI order (GeoNames 05 Chiapas, 06 Chihuahua,
07 Coahuila, 08 Colima; INEGI 05 Coahuila, 06 Colima, 07 Chiapas, 08 Chihuahua). The map was checked on
2026-10-31 against the capitals (PPLA/PPLC) of the MX dump and, for Durango (no PPLA row), against its ADM1 row.
"""

GEONAMES_ADMIN1_TO_STATE = {
    "01": "AGU", "02": "BCN", "03": "BCS", "04": "CAM", "05": "CHP", "06": "CHH", "07": "COA", "08": "COL",
    "09": "CMX", "10": "DUR", "11": "GUA", "12": "GRO", "13": "HID", "14": "JAL", "15": "MEX", "16": "MIC",
    "17": "MOR", "18": "NAY", "19": "NLE", "20": "OAX", "21": "PUE", "22": "QUE", "23": "ROO", "24": "SLP",
    "25": "SIN", "26": "SON", "27": "TAB", "28": "TAM", "29": "TLA", "30": "VER", "31": "YUC", "32": "ZAC",
}

# address_levels[1].value of Overture 2026-10-21.0, all 32 names that occur (count 2026-10-31).
OVERTURE_STATE_TO_CODE = {
    "Aguascalientes": "AGU", "Baja California": "BCN", "Baja California Sur": "BCS", "Campeche": "CAM",
    "Chiapas": "CHP", "Chihuahua": "CHH", "Coahuila de Zaragoza": "COA", "Colima": "COL",
    "Ciudad de México": "CMX", "Durango": "DUR", "Guanajuato": "GUA", "Guerrero": "GRO", "Hidalgo": "HID",
    "Jalisco": "JAL", "México": "MEX", "Michoacán de Ocampo": "MIC", "Morelos": "MOR", "Nayarit": "NAY",
    "Nuevo León": "NLE", "Oaxaca": "OAX", "Puebla": "PUE", "Querétaro": "QUE", "Quintana Roo": "ROO",
    "San Luis Potosí": "SLP", "Sinaloa": "SIN", "Sonora": "SON", "Tabasco": "TAB", "Tamaulipas": "TAM",
    "Tlaxcala": "TLA", "Veracruz de Ignacio de la Llave": "VER", "Yucatán": "YUC", "Zacatecas": "ZAC",
}
