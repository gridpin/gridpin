---
name: geocode-addresses
description: Turn addresses into coordinates or coordinates into addresses without sending them to an online service. Use when the user wants to geocode, reverse geocode, validate or map addresses, enrich a CSV or list with latitude and longitude, or asks for a geocoder that works offline or needs no API key.
---

Use the `gridpin` MCP tools. They run on this computer, and only `download_sheet` uses the network.

1. Work out which country the addresses are in. If they span several countries, group them and repeat these steps for each. Call `list_countries` to see what is covered, the size of each country's file and whether it is already downloaded. If the country is not covered, say so instead of guessing with another country's file.
2. If the file is missing, tell the user its size, then call `download_sheet`. The largest file is over 1 GB and can outlast a client's tool timeout. If that happens, have the user run `uvx --from gridpin-mcp==0.5.0 gridpin-fetch <country>` in a terminal, then call the tool again.
3. Call `geocode` for one address, `batch` for up to 1000 addresses per call, and `reverse` for coordinates. For a longer list, split it into chunks of 1000 and keep the input order. Pass each address as the person wrote it, including city and postcode when known.

Read the result before reporting it:

- `precision` says what the answer points to. `house` is the address itself, `interp` is estimated along the street, and `street` and `city` are only approximate. Report the level with the coordinates, and never present a street or city answer as the exact address.
- `weak: true` means the engine itself says the answer needs checking. List these rows for the user rather than dropping the flag.
- An empty result means no answer. Count and report these rows. Do not fill them in from memory.
- `reverse` is approximate: each result carries `distance_m` from the given point. Report that distance.

When you finish a list, give the counts by `precision`, plus the weak and empty counts, so the user can judge the result. Quality differs by country and by how messy the input is; measured results, including where other geocoders do better, are at https://gridpin.dev/bench.
