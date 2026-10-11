# Public quality benchmark

This file is the full version of the [benchmark on gridpin.dev](https://gridpin.dev/bench.html): the same
sets, with every count, the significance tests and the fingerprints of the data, so anyone can check the
numbers.

The question is always the same: **does the first answer land within 150 m of the true address?** An empty
answer counts as a miss. Each engine gets the identical query, one answer per query, one run per engine.

## How we measure

- **Independent truth.** The reference coordinate never comes from the data GridPin ships; a set whose truth
  copies any engine's source is thrown out before comparing.
- **Same input for everyone.** GridPin, Photon and Nominatim get the identical query. Nominatim is the public
  service, queried at most once per second. Photon is either the public service or a local instance of the
  country's OpenStreetMap extract — each country below says which.
- **No small samples as results.** Every set shown has at least 1,000 rows; empty answers stay in the
  denominator.
- **Mechanical distortions, not guesses.** Distortion rules are fixed before any engine answers and applied by
  a published hash, so no rule is tuned to a result.
- **Is the difference real?** Every comparison is checked with an exact paired two-sided McNemar test on the
  rows where only one of the two engines is right. A small *p* means the gap is beyond chance; a large *p*
  means this set cannot tell the two apart — which is **not proof of parity**.

## Germany · 2026-09-10 · 2 sets

**What we tested.** 2,000 real German addresses with known coordinates from two official registers — 1,000
public libraries (ISIL) and 1,000 electric-car charging stations (BNetzA) — in 15 of 16 Länder; **Bavaria is
not covered**. Rows were sealed on 2026-09-09, before the run, outside the development regression set and the
earlier 300-row sample. Each address was tested twice: as written (clean) and with one fixed mechanical
perturbation (dirty). The source families were used during development: **source-family independence is not
claimed**. This is registry-coordinate institution data, not a sample of human search traffic; charging-location
coordinates need not be building centroids.

**What it shows.** Clean addresses: GridPin 82.80%, Photon 83.15%, Nominatim 81.15% — GridPin and Photon are
level (not proof of parity); GridPin is ahead of Nominatim on this set. Dirty addresses: GridPin 82.10%, Photon
46.85%, Nominatim 15.85% — far beyond chance.

| Track | Engine | N | Found | Empty | ≤50 m | ≤150 m | ≤1,000 m | ≤5,000 m |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| clean | gridpin | 2000 | 1958 | 42 | 1280 | 1656 | 1851 | 1891 |
| clean | nominatim | 2000 | 1913 | 87 | 1241 | 1623 | 1853 | 1904 |
| clean | photon | 2000 | 1975 | 25 | 1250 | 1663 | 1907 | 1965 |
| dirty | gridpin | 2000 | 1953 | 47 | 1267 | 1642 | 1836 | 1876 |
| dirty | nominatim | 2000 | 376 | 1624 | 256 | 317 | 366 | 374 |
| dirty | photon | 2000 | 1266 | 734 | 710 | 937 | 1074 | 1105 |

Exact paired two-sided McNemar tests at ≤150 m:

| Track | Comparator | GridPin only | Comparator only | Both | Neither | p | Reading |
|---|---|---:|---:|---:|---:|---:|---|
| clean | Photon | 95 | 102 | 1,561 | 242 | 0.6691367 | level — not proof of parity |
| clean | Nominatim | 121 | 88 | 1,535 | 256 | 0.0266263 | GridPin ahead |
| dirty | Photon | 767 | 62 | 875 | 296 | 1.65976e-155 | GridPin ahead |
| dirty | Nominatim | 1,346 | 21 | 296 | 337 | 7.50908e-366 | GridPin ahead |

Tiny p-values are reported in decimal scientific notation rather than floating-point zero. Neither comparison
establishes performance for arbitrary German traffic.

| Source (1,000 rows each) | GridPin clean / dirty ≤150 m | Photon clean / dirty | Nominatim clean / dirty |
|---|---:|---:|---:|
| ISIL libraries | 803 / 793 | 820 / 464 | 804 / 148 |
| BNetzA chargers | 853 / 849 | 843 / 473 | 819 / 169 |

Services: local Photon 1.2.0 (import 2026-08-22T23:04:06Z) and the public Nominatim; each supplied 4,000
successful responses, no retries, with before/after status receipts. Engine selection was frozen at
`0e98c8ad3f66134b10d6a8659f15fe99dfa93076`. The measured sheet and the release sheet have identical
non-metadata sections.

<details><summary>Data fingerprints (SHA-256)</summary>

| Artifact | SHA-256 |
|---|---|
| Clean 2,000-row corpus | `9dd8e146b1c2fb90b028b0632b8a02d76c9273d821763ca01fff822fbc9a4f4b` |
| Dirty 2,000-row corpus | `0b1719b75f59553dbb8ff897e784fe93d0fdb8b172bc5c0dd4576781f22c0674` |
| Measured GridPin binary | `27d8301a4c1620a855da82d73f46168569562b66336f6b427a452f1b05081abb` |
| Measured sheet | `64d032374aa568e644375bca1d0cd36dff3d12b0f74842146dd503fa3feef464` |
| Release sheet (metadata-only repack, 2026-09-10) | `c0ac3d0cbf3dbd480bff6aebd717e2df96e9f0a2eb4e83aeffde736fa42b18af` |
| GridPin clean raw stream | `9aaf83f71bebc39f3c5513618f8f1642060e80a94e98e76f8ef7beab2aff39aa` |
| GridPin dirty raw stream | `ba1b15812c4b34f3aed01ea5feab75142a7bd20e66565c4a76fbaa1155eccf43` |
| Nominatim clean responses | `883998f211867a1d863dc7b730742557175f38e0cfe3fa3f948163fc9ebc0c77` |
| Nominatim dirty responses | `bce5b9940f5dd98ed09af2b91a0651bb3b06c1555b3aefd1e6bf046e1eb8f364` |
| Photon clean responses | `46dcc5d7bd2aba7e2754725cbe21933d2f01a18f00d79a5521a63134c611982f` |
| Photon dirty responses | `b5d1c2050a60706c02f053324ba96c15565d886a42da2ea070867e8edd5a004a` |

</details>

## Spain · 2026-09-28 · 2 sets

**What we tested.** Spain was measured on six comparable sets; this file shows two of them: the clean set where
GridPin is closest to the other services, and the mechanically distorted set where GridPin leads. **On the
other Spanish sets Photon leads.**

- **Andalusia schools, 2,000 rows (clean).** Official school registry of the Junta de Andalucía (CC BY 4.0),
  Andalusia only. Rows whose degree and UTM coordinates in the registry disagree by more than 1 m were excluded
  before sampling. The registry does not document how the point was placed. Truth passed the dependence check
  against all three engines (share of answers within 50 m and exact coordinate matches).
- **Distorted Wikidata, 1,590 rows.** Spanish Wikidata items with a coordinate (P625); each query is
  mechanically perturbed, the truth coordinate is kept byte for byte. The parent queries include locality
  descriptions, so this is not a set of canonical addresses and not observed human noise. One parent row outside
  the Spanish country contour (Overture 2026-08-19.0, land and maritime union) is excluded equally for all three
  engines.

**What it shows.** Distorted addresses: GridPin 33.84%, Photon 29.56%, Nominatim 20.82% — GridPin ahead of
both, beyond chance, though by less than in Germany. Andalusia schools: Photon 63.50% leads; GridPin 59.20% and
Nominatim 60.80% are level (not proof of parity).

| Set | Engine | N | Found | Empty | ≤50 m | ≤150 m | ≤1,000 m | ≤5,000 m |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| andalusia | gridpin | 2000 | 1802 | 198 | 887 | 1184 | 1490 | 1638 |
| andalusia | nominatim | 2000 | 1666 | 334 | 700 | 1216 | 1568 | 1613 |
| andalusia | photon | 2000 | 1938 | 62 | 726 | 1270 | 1760 | 1894 |
| distorted | gridpin | 1590 | 1056 | 534 | 385 | 538 | 733 | 953 |
| distorted | nominatim | 1590 | 511 | 1079 | 226 | 331 | 443 | 465 |
| distorted | photon | 1590 | 875 | 715 | 347 | 470 | 626 | 692 |

Exact paired two-sided McNemar tests at ≤150 m:

| Set | Comparator | GridPin only | Comparator only | Both | Neither | p | Reading |
|---|---|---:|---:|---:|---:|---:|---|
| andalusia | Nominatim | 377 | 409 | 807 | 407 | 0.268831 | level — not proof of parity |
| andalusia | Photon | 358 | 444 | 826 | 372 | 0.00266489 | Photon ahead |
| distorted | Nominatim | 340 | 133 | 198 | 919 | 5.90642e-22 | GridPin ahead |
| distorted | Photon | 297 | 229 | 241 | 823 | 0.00344627 | GridPin ahead |

Services: GridPin ran `batch -k 1`, one thread, on 2026-09-28 against the release sheet `spain.bin` (v0.3.0).
Andalusia competitors, 2026-09-25: public Nominatim 5.3.0 (`data_updated` 2026-09-25T02:01:59Z before and
after) and public Photon 1.3.0 (import 2026-09-19T23:03:26Z). Distorted competitors, 2026-09-21: public
Nominatim 5.3.0 (`data_updated` 2026-09-21T20:49:05Z before and after) and local Photon 1.2.0 (import
2026-09-12T23:03:36Z, Spain extract). Competitor answers were collected once and are not re-run for later
GridPin builds.

<details><summary>Data fingerprints (SHA-256)</summary>

| Artifact | SHA-256 |
|---|---|
| Measured GridPin binary | `9df585faa43b6766e29e14ea77b7153b3d7ae57d39665a5498b7eb8052e32f99` |
| Measured sheet = release `spain.bin` v0.3.0 | `c671b9a46089e32580189ee3c5a65e024779691a50ab26e77f5639023994819b` |
| Andalusia queries | `5340fa7758f3e4dfb635aca80d6643b5912bbe63e0e0d9a7750f00366ba3f523` |
| Andalusia truth | `120fcb317d64960e47029ba88f491efb2d6a63e978a9ffc054a017b236198633` |
| Andalusia GridPin / Nominatim / Photon raw | `ac2a65c770d6babf90859b6e11712f4c3ad91719651b494ed2d2bcf1d59270bf` / `a9e46f982f399228d085bb22aed4e12e609e58bc769eb39539df252650711ed0` / `0e67abcfd7ab18f91bc808a93c0f06bff14d2fa4f68e805525a5c810cc63b979` |
| Distorted queries | `48502015916d75cee0136957f4b3ce418e2f2f6c2b0ccd65a9576537c19cb374` |
| Distorted truth | `cfd8e8f4bca7e9e85be3b894dd2387240360390fea318bc8338985d572ad9b08` |
| Distorted GridPin / Nominatim / Photon raw | `bca381b971524c497325fc66e731ce68faccf9e22da305b7f85ad56be606fa74` / `ee2d2a4f1f40ea2da19e2c748eb8fb63ea60b9782b517c75095745d22ae7d4f2` / `bbd1cda52929e9e9002d1ed44a53d5014cad8342144a3a9a1faa33adf3a6f824` |

</details>

## Brazil · 2026-10-06 · 3 sets

**What we tested.** Three retained sets with known coordinates: clean Wikidata and mechanically distorted
Wikidata (2,376 rows each) and the CNES health-facility register as typed (1,000). Two clean rows outside the
country contour are excluded equally for all three engines. These sets are not observed human search traffic.

**What it shows.** Distorted addresses: GridPin 20.71% and Photon 19.82% are level (not proof of parity), both
well ahead of Nominatim 9.64%. Health-facility register: GridPin 36.20% leads both Photon 28.20% and Nominatim
20.70%, beyond chance. Clean addresses: on clean Brazilian addresses Photon leads too (25.5% vs 23.2%).

| Set | Engine | N | Found | Empty | ≤50 m | ≤150 m | ≤1,000 m | ≤5,000 m |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| clean | gridpin | 2376 | 1566 | 810 | 272 | 552 | 968 | 1249 |
| clean | nominatim | 2376 | 1009 | 1367 | 172 | 412 | 783 | 884 |
| clean | photon | 2376 | 1494 | 882 | 311 | 607 | 1114 | 1271 |
| distorted | gridpin | 2376 | 1399 | 977 | 231 | 492 | 847 | 1097 |
| distorted | nominatim | 2376 | 514 | 1862 | 100 | 229 | 413 | 461 |
| distorted | photon | 2376 | 1206 | 1170 | 241 | 471 | 879 | 996 |
| cnes | gridpin | 1000 | 751 | 249 | 267 | 362 | 596 | 699 |
| cnes | nominatim | 1000 | 512 | 488 | 122 | 207 | 449 | 504 |
| cnes | photon | 1000 | 802 | 198 | 160 | 282 | 643 | 756 |

Exact paired two-sided McNemar tests at ≤150 m:

| Set | Comparator | GridPin only | Comparator only | Both | Neither | p | Reading |
|---|---|---:|---:|---:|---:|---:|---|
| distorted | Nominatim | 373 | 110 | 119 | 1,774 | 1.72106e-34 | GridPin ahead |
| distorted | Photon | 276 | 255 | 216 | 1,629 | 0.385453 | level — not proof of parity |
| cnes | Nominatim | 243 | 88 | 119 | 550 | 6.2216e-18 | GridPin ahead |
| cnes | Photon | 199 | 119 | 163 | 519 | 8.49867e-06 | GridPin ahead |

Services: public Nominatim and public Photon, answers collected 2026-09-30 and retained; no new competitor
requests were made for the GridPin build measured on 2026-10-06. Top-1 great-circle distance to independent
truth, computed with `wikidata4_regress.py`.

<details><summary>Data fingerprints (SHA-256)</summary>

| Artifact | SHA-256 |
|---|---|
| Measured GridPin binary | `84bd75bc3264e71df57d3a76c9d69575ccaa24bcc4cd56975eb353830740cf4f` |
| Measured sheet `br.bin` | `2d415c3a2abfd2df749b6a1d58625c12f8df1328a56248c9bc5eb5628cbc3ac7` |
| Release sheet `brazil.bin` (v0.4.0; metadata-only repack of the measured sheet, 2026-10-10) | `016e102debb05aafad0ae409c3a2acec478b60bb48b74d03210b80dc52d42a6d` |
| clean queries | `9809af6379a64e512768fbabb803fa877e8bf5ecb1ee8d917bba87ac849c7620` |
| clean truth | `1e56d8f5ad0b373846fdda874a9c3ddce42d11f38b4b0d9486b32fe8cfd7b79d` |
| clean gridpin raw | `15514dfa7e48dd18aafeaf8ab7b02626ab71090894c7c57873c247f70e8a36e2` |
| clean nominatim raw | `e9162abee36a515130cf23f1d68d1550189ab959de110b271a9bd5c75465293b` |
| clean photon raw | `031724c133299070a2fcec610474525d94b3fea4c1d55c50f69795db092a0f0f` |
| distorted queries | `3c18a1bb0af824b376a9e49f3ef5d61da9da6c82dcc6d46ad4866270a0d63d83` |
| distorted truth | `81d84bc238830b9f3fabdcd8bf71f2c9d9d7e11c970a3ed079c362902b8d357e` |
| distorted gridpin raw | `fe1e572f68c529575c6770cfa373fb0ca2183b43fb722871e6eeb422f24ce93c` |
| distorted nominatim raw | `7a6634335d3a14412d3ea8b3f5d5b4315546f7953578414f3c6edbc0c41d1acd` |
| distorted photon raw | `04c86c42aaacc64149742891e4355303240b173084f0ceaf0564ec3476e3d390` |
| cnes queries | `4517fba882a2c95df9d6d6afaae83c45dffcc3215b7e9773403cbda32bf9ab00` |
| cnes truth | `79d087f40a5f9a6279006112db497ea4faa5d9efedb85be388b11b6113ce4e93` |
| cnes gridpin raw | `0536f5f3b11d40c1e88b8b0eadcb44c3240223f054c0f448e0cc295ae1cf6d28` |
| cnes nominatim raw | `83fa56d942b394cf9a4a880e19ea47038869b3b2a83f44290b924d4f8243f4d1` |
| cnes photon raw | `103ff5946a43fba642ada1f3ba56555a057eb570083bb8bcb3240dd429f6570c` |

</details>

## France, Italy, the Netherlands, Serbia

Covered by the free sheets. No independent comparison with Photon and Nominatim on a set of at least 1,000 rows
has been made for them yet, so none is shown here.

## What these numbers do not say

- None of these sets is a sample of real human search traffic; the results describe these sets only, not
  arbitrary queries.
- A *level* result is not proof of parity — only that this set cannot tell the two engines apart.
- The query sets and raw answers are retained evidence and are not bundled in the public source checkout; the
  fingerprints above identify them exactly.
