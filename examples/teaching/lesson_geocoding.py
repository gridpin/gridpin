"""A one-hour lesson: what a geocoder does, and how to tell a good answer from a lucky one.

Runs offline on the Monaco sheet (a few MB): `python lesson_geocoding.py monaco.bin`. Each step prints a question
for the class and the numbers that answer it. No account, no network, no API key.

1. Exact address -> a house point and its match level.
2. The same address written badly (case, accents, word order) -> does the answer move?
3. A street with no such number -> what does the engine say, and why is "street" honest?
4. Distance: how far is the answer for a missing number from the house? (the haversine the project uses, R = 6 371 000 m)
5. Reverse: a point -> the nearest address.
"""
import math
import sys

import gridpin

R = 6_371_000.0


def distance_m(a, b):
    la1, lo1, la2, lo2 = map(math.radians, (a[0], a[1], b[0], b[1]))
    h = math.sin((la2 - la1) / 2) ** 2 + math.cos(la1) * math.cos(la2) * math.sin((lo2 - lo1) / 2) ** 2
    return 2 * R * math.asin(math.sqrt(h))


def first(engine, q):
    hits = engine.geocode(q, k=1)
    return hits[0] if hits else None


def lesson(sheet):
    g = gridpin.Geocoder(sheet)
    exact = first(g, "4 avenue de la Madone, Monaco")
    print("1. exact:", exact and (exact["lat"], exact["lon"], exact["match_level"]))
    messy = first(g, "MONACO  avenue de la madone 4")
    if exact and messy:
        d = distance_m((exact["lat"], exact["lon"]), (messy["lat"], messy["lon"]))
        print(f"2. written badly: {messy['match_level']}, {d:.1f} m from the exact answer")
    nonumber = first(g, "999 avenue de la Madone, Monaco")
    print("3. no such number:", nonumber and nonumber["match_level"])
    if exact and nonumber:
        print(f"4. street vs house: {distance_m((exact['lat'], exact['lon']), (nonumber['lat'], nonumber['lon'])):.0f} m")
    if exact:
        back = g.reverse(exact["lat"], exact["lon"], k=1)
        print("5. reverse:", back and f"{back[0]['housenumber']} {back[0]['street']}, {back[0]['distance_m']} m")
    return exact, messy, nonumber


if __name__ == "__main__":
    lesson(sys.argv[1] if len(sys.argv) > 1 else "monaco.bin")
