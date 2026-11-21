# gridpin-photos

**Nothing leaves your machine.** The GPS point of each photo is read on this computer and looked up in a GridPin file
on this computer; no address, point or photo is sent anywhere.

`gridpin-photos` writes a caption of place for every photo of a folder — street, town, region, and how far the nearest
address is — into a CSV beside the photos, and, if asked, an `.xmp` sidecar per photo that photo managers read. The
photos themselves are only read, never written.

```bash
pip install "gridpin-photos @ git+https://github.com/gridpin/gridpin@v0.10.0#subdirectory=integrations/photos"
python -c 'import gridpin; print(gridpin.sheets.fetch("fr"))'      # the country file, once
gridpin-photos ~/Pictures/2026-paris --sheet fr --xmp
```

```text
$ gridpin-photos louvre --sheet france.bin --xmp
4 photos: 2 ok, 2 no_geotag
$ cat louvre/gridpin_places.csv
file,lat,lon,status,label,street,housenumber,place,region,postcode,distance_m
fr_27456425.jpg,,,no_geotag,,,,,,,
fr_45674879.jpg,,,no_geotag,,,,,,,
fr_53217347.jpg,48.860600,2.337638,ok,"91ba Rue de Rivoli, Paris 1er Arrondissement",Rue de Rivoli,91ba,Paris 1er Arrondissement,,75001,90
fr_53217571.jpg,48.860600,2.337638,ok,"91ba Rue de Rivoli, Paris 1er Arrondissement",Rue de Rivoli,91ba,Paris 1er Arrondissement,,75001,90
```

(Four photos of the Louvre from Wikimedia Commons: two carry a GPS block, two were placed on a map by hand and carry
none — `no_geotag` is the photo's own state, not a failure.)

| status | meaning |
|---|---|
| `ok` | the nearest address is close enough (150 m, `--max-street-m`) to name the street |
| `place_only` | the nearest house is farther, or the answer is weak: only the town is named — a photo from a bridge or a park does not get a street it was not taken on |
| `no_geotag` | no GPS position in the photo (or a block that cannot be read) |
| `null_island` | the position is 0, 0: what a camera without a fix writes |
| `no_address` | no address of the file near the point: another country, the sea |

The `.xmp` sidecar (`IMG_0412.jpg.xmp`) carries `photoshop:City`, `photoshop:State` and `Iptc4xmpCore:Location`; one
that is already there is never overwritten. JPEG, TIFF, HEIC/HEIF, PNG and WebP are read (exifread). One country file
per run: photos from another country come out `no_address`.

For a photo server (Immich, PhotoPrism, Nextcloud) there is a guide on gridpin.dev; this package is for a plain folder.

## Tests

`make test-photos` (from `code/`): photos built byte by byte with GPS blocks of both hemispheres, a 0, 0 point, none,
a broken block; the run must leave every photo byte for byte as it was.
