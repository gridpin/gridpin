# Public quality benchmark

This file is the full version of the [benchmark on gridpin.dev](https://gridpin.dev/bench): the same
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

**What it shows.** Distorted addresses: GridPin 34.03%, Photon 29.56%, Nominatim 20.82% — GridPin ahead of
both, beyond chance, though by less than in Germany. Andalusia schools: Photon 63.50% leads; GridPin 59.20% and
Nominatim 60.80% are level (not proof of parity).

| Set | Engine | N | Found | Empty | ≤50 m | ≤150 m | ≤1,000 m | ≤5,000 m |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| andalusia | gridpin | 2000 | 1802 | 198 | 887 | 1184 | 1490 | 1638 |
| andalusia | nominatim | 2000 | 1666 | 334 | 700 | 1216 | 1568 | 1613 |
| andalusia | photon | 2000 | 1938 | 62 | 726 | 1270 | 1760 | 1894 |
| distorted | gridpin | 1590 | 1051 | 539 | 387 | 541 | 741 | 961 |
| distorted | nominatim | 1590 | 511 | 1079 | 226 | 331 | 443 | 465 |
| distorted | photon | 1590 | 875 | 715 | 347 | 470 | 626 | 692 |

Exact paired two-sided McNemar tests at ≤150 m:

| Set | Comparator | GridPin only | Comparator only | Both | Neither | p | Reading |
|---|---|---:|---:|---:|---:|---:|---|
| andalusia | Nominatim | 377 | 409 | 807 | 407 | 0.268831 | level — not proof of parity |
| andalusia | Photon | 358 | 444 | 826 | 372 | 0.00266489 | Photon ahead |
| distorted | Nominatim | 342 | 132 | 199 | 917 | 1.58319e-22 | GridPin ahead |
| distorted | Photon | 299 | 228 | 242 | 821 | 0.00226359 | GridPin ahead |

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

**What it shows.** Distorted addresses: GridPin 20.79% and Photon 19.82% are level (not proof of parity), both
well ahead of Nominatim 9.64%. Health-facility register: GridPin 36.80% leads both Photon 28.20% and Nominatim
20.70%, beyond chance. Clean addresses: on clean Brazilian addresses Photon leads too (25.5% vs 23.3%).

| Set | Engine | N | Found | Empty | ≤50 m | ≤150 m | ≤1,000 m | ≤5,000 m |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| clean | gridpin | 2376 | 1549 | 827 | 271 | 554 | 974 | 1252 |
| clean | nominatim | 2376 | 1009 | 1367 | 172 | 412 | 783 | 884 |
| clean | photon | 2376 | 1494 | 882 | 311 | 607 | 1114 | 1271 |
| distorted | gridpin | 2376 | 1370 | 1006 | 230 | 494 | 847 | 1094 |
| distorted | nominatim | 2376 | 514 | 1862 | 100 | 229 | 413 | 461 |
| distorted | photon | 2376 | 1206 | 1170 | 241 | 471 | 879 | 996 |
| cnes | gridpin | 1000 | 752 | 248 | 270 | 368 | 604 | 707 |
| cnes | nominatim | 1000 | 512 | 488 | 122 | 207 | 449 | 504 |
| cnes | photon | 1000 | 802 | 198 | 160 | 282 | 643 | 756 |

Exact paired two-sided McNemar tests at ≤150 m:

| Set | Comparator | GridPin only | Comparator only | Both | Neither | p | Reading |
|---|---|---:|---:|---:|---:|---:|---|
| distorted | Nominatim | 374 | 109 | 120 | 1,773 | 5.03758e-35 | GridPin ahead |
| distorted | Photon | 278 | 255 | 216 | 1,627 | 0.340635 | level — not proof of parity |
| cnes | Nominatim | 246 | 85 | 122 | 547 | 2.71682e-19 | GridPin ahead |
| cnes | Photon | 202 | 116 | 166 | 516 | 1.63145e-06 | GridPin ahead |

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

## United States · 2026-10-23 · 3 sets

**What we tested.** Three retained sets with known coordinates: clean Wikidata and mechanically distorted Wikidata
(3,655 rows each) and the National Register of Historic Places as recorded (1,000). Rows outside the 50 states and
DC are excluded equally for all three engines. These sets are not observed human search traffic.

**What it shows.** Distorted addresses: GridPin 37.37% leads Photon 28.45% and Nominatim 24.32%, beyond chance.
Historic-places register: GridPin 64.10% leads Photon 38.90% and Nominatim 52.30%, beyond chance. Clean addresses:
on clean US addresses Nominatim and Photon both lead (42.5% and 41.3% vs 39.2%). One nationwide sheet with point
addresses in 35 states and Census address ranges elsewhere; range answers are interpolated.

| Set | Engine | N | Found | Empty | ≤50 m | ≤150 m | ≤1,000 m | ≤5,000 m |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| clean | gridpin | 3655 | 3476 | 179 | 847 | 1432 | 2103 | 2296 |
| clean | nominatim | 3655 | 3222 | 433 | 991 | 1553 | 2192 | 2338 |
| clean | photon | 3655 | 3490 | 165 | 1010 | 1510 | 2224 | 2482 |
| distorted | gridpin | 3655 | 3365 | 290 | 809 | 1366 | 1981 | 2166 |
| distorted | nominatim | 3655 | 1796 | 1859 | 571 | 889 | 1330 | 1427 |
| distorted | photon | 3655 | 2864 | 791 | 698 | 1040 | 1601 | 1804 |
| nrhp | gridpin | 1000 | 895 | 105 | 468 | 641 | 763 | 825 |
| nrhp | nominatim | 1000 | 810 | 190 | 359 | 523 | 707 | 774 |
| nrhp | photon | 1000 | 880 | 120 | 271 | 389 | 642 | 783 |

Exact paired two-sided McNemar tests at ≤150 m:

| Set | Comparator | GridPin only | Comparator only | Both | Neither | p | Reading |
|---|---|---:|---:|---:|---:|---:|---|
| clean | Nominatim | 350 | 471 | 1,082 | 1,752 | 2.72734e-05 | Nominatim ahead |
| clean | Photon | 476 | 554 | 956 | 1,669 | 0.0163881 | Photon ahead |
| distorted | Nominatim | 707 | 230 | 659 | 2,059 | 4.67802e-57 | GridPin ahead |
| distorted | Photon | 670 | 344 | 696 | 1,945 | 7.33638e-25 | GridPin ahead |
| nrhp | Nominatim | 176 | 58 | 465 | 301 | 5.22407e-15 | GridPin ahead |
| nrhp | Photon | 309 | 57 | 332 | 302 | 5.28401e-43 | GridPin ahead |

Services: public Nominatim and public Photon, answers collected 2026-10-20 and retained; no new competitor
requests were made for the GridPin build measured on 2026-10-23. Top-1 great-circle distance to independent
truth, computed with `wikidata4_regress.py`.

<details><summary>Data fingerprints (SHA-256)</summary>

| Artifact | SHA-256 |
|---|---|
| Measured GridPin binary | `f2dea2f0b9dc124b9342cb7c404b932c607af92bd77059ce9addb582d9bc5926` |
| Measured sheet `us.bin` | `8fbdf76105057eab264dd40d37e080eeb506c09f23562718b9173035cae1a258` |
| Release sheet `usa.bin` (v0.5.0; metadata-only repack of the measured sheet, 2026-10-23) | `4f3422f4d95939aefbcf454ceebc9a8dd16f78f2a13c5e6660028ddfad712cc0` |
| clean queries | `f5e07a251a447508482fa1b9ead94c4c662d0f6c8f91a28e9eab5b1cf054549e` |
| clean truth | `a7ba4d3492c63372af5af98f6c791e1fa0eb83e61845f528596a944d7bae3dd9` |
| clean gridpin raw | `171f3aa9cc0b6add10521b5d79296216a640c5e0875c9fe29b49342c3944d91f` |
| clean nominatim raw | `b88906d62ac4ba2104ad335e540a1d1d328edcdb9e0eeb924d8bfd4164d9b719` |
| clean photon raw | `e2d2c6cfdd065177895c7a9829c1e147f6b34115d06cfc869dcf26551dfda642` |
| distorted queries | `4159ca5da2e1945327268e89d5a7a7470c07b1e31811478887ce57ab4e14fc01` |
| distorted truth | `5a3e433784eb574b016e72704d4add23013f322cde24e78b5ea5b7e3acc26932` |
| distorted gridpin raw | `a7371389048d8165129e67f311ae0e602e770dccf775f3361dafbda10ea82b26` |
| distorted nominatim raw | `1aa04709b79dea7863559cdece240927926f8d8ea38fb9e6dc3078431bab557e` |
| distorted photon raw | `e5d9d74c3c0f869cf22688c77945259c0725add1d3f4bb2ee6c318192b224c2f` |
| nrhp queries | `661e364034b7efa66323d7f050840c14ec3013b628ae4b4d924a9e6fff901ce1` |
| nrhp truth | `d8db31e9c8b7286005589436f41177fca5c3fcfc2da829582cd1cddd88b5ec50` |
| nrhp gridpin raw | `cca0515929ea29c3ac7005e09e9210e6c8de07585e9c7cd49e5b6ef8b4a39b9c` |
| nrhp nominatim raw | `45fe8cf55be0df4578061d3c793126fe4cff0d4a329365b7104923560dcdcead` |
| nrhp photon raw | `e4b3c98476ac16c529117920d3d261f04e42e32ecbb4c82f6b79a1f59266659c` |

</details>

## Canada · 2026-10-30 · 3 sets

**What we tested.** Three retained sets with known coordinates: clean Wikidata and mechanically distorted Wikidata
(2,785 rows each; three rows outside Canada excluded equally for all three engines) and 1,000 radio-station sites
from the ISED Technical and Administrative Frequency List, as recorded. These sets are not observed human search
traffic.

**What it shows.** Distorted addresses: GridPin 55.98% leads Photon 45.21% and Nominatim 32.68%, beyond chance.
Radio-station sites: GridPin 39.30% and Photon 39.30% are level (not proof of parity), both ahead of Nominatim
25.20%. Clean addresses: on clean Canadian addresses Photon leads (59.1% vs 57.5%; Nominatim 57.2%); the set cannot tell the three apart beyond chance. One
nationwide sheet with point addresses in all provinces and territories; answers carry the Canadian postal code.

| Set | Engine | N | Found | Empty | ≤50 m | ≤150 m | ≤1,000 m | ≤5,000 m |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| clean | gridpin | 2785 | 2703 | 82 | 1060 | 1602 | 1951 | 2048 |
| clean | nominatim | 2785 | 2587 | 198 | 1011 | 1593 | 1912 | 2035 |
| clean | photon | 2785 | 2751 | 34 | 1038 | 1646 | 1988 | 2110 |
| distorted | gridpin | 2785 | 2664 | 121 | 1033 | 1559 | 1899 | 2001 |
| distorted | nominatim | 2785 | 1493 | 1292 | 602 | 910 | 1109 | 1185 |
| distorted | photon | 2785 | 2453 | 332 | 827 | 1259 | 1535 | 1652 |
| tafl | gridpin | 1000 | 809 | 191 | 184 | 393 | 623 | 699 |
| tafl | nominatim | 1000 | 605 | 395 | 131 | 252 | 425 | 537 |
| tafl | photon | 1000 | 806 | 194 | 204 | 393 | 607 | 696 |

Exact paired two-sided McNemar tests at ≤150 m:

| Set | Comparator | GridPin only | Comparator only | Both | Neither | p | Reading |
|---|---|---:|---:|---:|---:|---:|---|
| clean | Nominatim | 393 | 384 | 1,209 | 799 | 0.774135 | level |
| clean | Photon | 349 | 393 | 1,253 | 790 | 0.114372 | level |
| distorted | Nominatim | 868 | 219 | 691 | 1,007 | 7.51195e-92 | GridPin ahead |
| distorted | Photon | 626 | 326 | 933 | 900 | 1.47901e-22 | GridPin ahead |
| tafl | Nominatim | 200 | 59 | 193 | 548 | 4.10126e-19 | GridPin ahead |
| tafl | Photon | 120 | 120 | 273 | 487 | 1 | level |

Services: public Nominatim and public Photon, answers collected 2026-10-28 and retained; no new competitor
requests were made for the GridPin build measured on 2026-10-30. Top-1 great-circle distance to independent
truth, computed with `wikidata4_regress.py`.

<details><summary>Data fingerprints (SHA-256)</summary>

| Artifact | SHA-256 |
|---|---|
| Measured GridPin binary | `f2dea2f0b9dc124b9342cb7c404b932c607af92bd77059ce9addb582d9bc5926` |
| Measured sheet `ca.bin` | `fcd4d9a9d1c5a1540722a66e7cad470ec515525f333df5f9ddfddea337449b44` |
| Release sheet `canada.bin` (v0.6.0; metadata-only repack of the measured sheet, 2026-10-30) | `234515843c54b6c4feb93ab83c7b023a3e14beec8357c6d570eeed0f66f35400` |
| clean queries | `401609e84a568e00bd430f2491431d414635988c54b6191df5242d54b7a8be40` |
| clean truth | `3ff8483c360b597a95b2da76cc29d22311f99bc6b6c2e550b8ee06f92c1e0153` |
| clean gridpin raw | `c6a47b61d14db89d285dde66ac5c901edf8a1f3fcd54af127a950bf255594683` |
| clean nominatim raw | `222b8a48d3e87f406e0e9b1764e6c79339c7da358d673fba6a111561f8c39477` |
| clean photon raw | `8609334d60f7cc673d83dde7898f59bccdd46551dea57609439cf9306ffa5568` |
| distorted queries | `e705a0f57d7423c49c7f66fd4fa06ea5083c744fedae226be46c7ece60557434` |
| distorted truth | `c06654e2f12da2f88fd1ea6e0e3e3f9c5a485c9f5e6922ff8a0469c0d01e41bb` |
| distorted gridpin raw | `da526f9c0245a21691d79c3618148621ac60574c237f04783ff2cfcd036b9e0f` |
| distorted nominatim raw | `ea8dc780de77735515532febb8cfd0cc4c110a085341d844e9958fd914766afb` |
| distorted photon raw | `eb8517e1452087ea4b622d4b6a558fc4edfb0732731a912f27202ba88af54015` |
| tafl queries | `f913520c7f93508606b9974a7a9fb6bd40209264ff4a570f111aa8a5c5127262` |
| tafl truth | `b0e00a314851314ef31720e380025a1dff06e6df9bd4c6aaa1dcbb188d78b605` |
| tafl gridpin raw | `74c865d84f0cf67127983b69d90b7de59b84ddfbcd55fb7ee7ad9e460805646c` |
| tafl nominatim raw | `33100eeed662f19d33b24981f371993d823cd9bb60faa2befc4ebd374c7bed1c` |
| tafl photon raw | `2ba010a9bfd13122223d09c2c79d5fd074955f47c9ed686e4ea4c76fdf1e35f3` |

</details>

## Mexico · 2026-11-04 · 3 sets

**What we tested.** Three retained sets with known coordinates: 2,000 clean addresses from two independent sources
(909 places from Wikidata and 1,091 health facilities from the CLUES register of the Secretaría de Salud), the same
2,000 mechanically distorted, and 1,000 cultural venues from the Sistema de Información Cultural (Secretaría de
Cultura), addresses as the venues write them. Rows whose point lies outside Mexico were removed before selection.
These sets are not observed human search traffic.

**What it shows.** Distorted addresses: GridPin 24.10% leads Photon 13.80% and Nominatim 7.65%, beyond chance.
Cultural venues: GridPin 28.00% leads Photon 17.30% and Nominatim 10.20%, beyond chance. Clean addresses: on clean
Mexican addresses GridPin leads (29.4% vs Photon 15.9%, Nominatim 10.1%); the set has 2,000 rows, so the margin of error
is about ±2 points. The address layer was surveyed in 2009–2010, so newer addresses are missing from it. One
nationwide sheet; answers carry the postal code where the source has one.

| Set | Engine | N | Found | Empty | ≤50 m | ≤150 m | ≤1,000 m | ≤5,000 m |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| clean | gridpin | 2000 | 1247 | 753 | 437 | 587 | 852 | 1013 |
| clean | nominatim | 2000 | 668 | 1332 | 110 | 202 | 416 | 462 |
| clean | photon | 2000 | 1046 | 954 | 186 | 318 | 697 | 818 |
| distorted | gridpin | 2000 | 1079 | 921 | 357 | 482 | 678 | 814 |
| distorted | nominatim | 2000 | 487 | 1513 | 84 | 153 | 302 | 340 |
| distorted | photon | 2000 | 1026 | 974 | 154 | 276 | 631 | 757 |
| sic | gridpin | 1000 | 616 | 384 | 200 | 280 | 422 | 533 |
| sic | nominatim | 1000 | 202 | 798 | 62 | 102 | 164 | 188 |
| sic | photon | 1000 | 406 | 594 | 102 | 173 | 324 | 370 |

Exact paired two-sided McNemar tests at ≤150 m:

| Set | Comparator | GridPin only | Comparator only | Both | Neither | p | Reading |
|---|---|---:|---:|---:|---:|---:|---|
| clean | Nominatim | 473 | 88 | 114 | 1,325 | 1.05138e-64 | GridPin ahead |
| clean | Photon | 433 | 164 | 154 | 1,249 | 5.98336e-29 | GridPin ahead |
| distorted | Nominatim | 412 | 83 | 70 | 1,435 | 1.89426e-53 | GridPin ahead |
| distorted | Photon | 385 | 179 | 97 | 1,339 | 2.53319e-18 | GridPin ahead |
| sic | Nominatim | 216 | 38 | 64 | 682 | 2.09717e-31 | GridPin ahead |
| sic | Photon | 200 | 93 | 80 | 627 | 3.80807e-10 | GridPin ahead |

Services: public Nominatim and public Photon; answers for the clean and distorted sets collected 2026-11-04, for
cultural venues 2026-10-31; all retained. Top-1 great-circle distance to independent truth, computed with
`wikidata4_regress.py`.

<details><summary>Data fingerprints (SHA-256)</summary>

| Artifact | SHA-256 |
|---|---|
| Measured GridPin binary | `09238e49275faf57e57790a77c10f5426bbef709c7b3893278194edb21483a03` |
| Release sheet `mexico.bin` (v0.7.0), clean and distorted sets | `6cc9e627bf84cf1126a5d31b71c14504cbbbf56eb5b1835731d85fc2a2ef454c` |
| Sheet `mx.bin` before the metadata-only repack, cultural venues | `c87ba719919e37ed80f8a9166d1b13f5e4be7e8a3ed272c923bc3bf653826110` |
| clean queries | `a0a3b182411f5b413d035e08b78472e2639b9e02f1cf3c7a43fb66a1fcaab443` |
| clean truth | `350a0cd4eb5daf2968fbcf59c12ecb6785622815c4d92f2e6ca8298f682a7f1c` |
| clean gridpin raw | `96efa22adca3dec94533ee7e26d96ee4d1c8e1a0ef37716ddb068098f182b444` |
| clean nominatim raw | `a307fbfabac5be3c8e923159ec454cb23807ae9ae1199030cdcbe43eba47c571` |
| clean photon raw | `6e1377083fa8ad97a1d6b5ffe8b28a2d1bb5d07ab6ee7116a077e9a8de433a27` |
| distorted queries | `563e57aef3a6297116a6f4dfa779603045c6e388eac57bc3e59cfceec71c0aef` |
| distorted truth | `0d770b0089961a5c4564f999b42a3b1405f90ee36905eebcea98a4b1953567d6` |
| distorted gridpin raw | `2b2af02cd9dd75d16ab90c2ead5f3094cb39fb74c72591ee345000b0549bc0fc` |
| distorted nominatim raw | `8b5f9825cd10ff3321ade1aeddad627390e6d32bf4723ce9419aa9081823308e` |
| distorted photon raw | `cb747241c64e9d7c4c295d1d96f694c0c2e7204092941a718f35114c7125d59d` |
| sic queries | `c6b28ada653559a10c756971d48460c1c72a48661aab83a85fc0fca3c2f3a8e7` |
| sic truth | `e5a70f5fc35fd8e3bfdba0208527821e40cf0ea0309a7a22c6f5e39232ba03ec` |
| sic gridpin raw | `91b222dd23fb366ae22ae5ae250009f50c32d03f35893a1c24214703e5093bb2` |
| sic nominatim raw | `6f7e368f7ad30d950b88852fa0459391333a986b1ebfaa0a31e4c549efaf0407` |
| sic photon raw | `dd05add51b6f8609201df174155812554e44fed6333e1194fa9707bbc1a34ffe` |

</details>

## Australia · 2026-11-05 · 3 sets

**What we tested.** Three sets with known coordinates: 2,000 clean addresses (1,327 regular polling places of the 2025 federal election from the Australian Electoral Commission, 673 places from Wikidata), the same 2,000 with one mechanical distortion each, and 1,000 radio sites from the ACMA licence register, addresses as the licensees write them (New South Wales only). Wikidata rows whose coordinate cites a heritage register are excluded: those registers share their points with the address data. These sets are not observed human search traffic.

**What it shows.** Distorted addresses: GridPin leads both (74.6% vs 50.6% and 42.2%), beyond chance. Radio sites: GridPin leads both (65.8% vs 25.2% and 13.0%), beyond chance. Clean addresses: GridPin leads both (75.0% vs 54.1% and 53.2%), on polling places 83.8% vs 59.0% and 59.6%, on Wikidata 57.7% vs 44.4% and 40.6%. Polling places are mostly schools and halls: the reference point is the building, the sheet's point is the address on the lot, so a correct answer can stand beyond 150 m. The AEC does not document how it places points; independence of the kept points was measured, not documented.

| Set | Engine | N | Found | Empty | ≤50 m | ≤150 m | ≤1,000 m | ≤5,000 m |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| clean | gridpin | 2000 | 1990 | 10 | 858 | 1500 | 1859 | 1925 |
| clean | nominatim | 2000 | 1888 | 112 | 494 | 1064 | 1673 | 1824 |
| clean | photon | 2000 | 1889 | 111 | 525 | 1082 | 1668 | 1828 |
| distorted | gridpin | 2000 | 1987 | 13 | 854 | 1492 | 1851 | 1921 |
| distorted | nominatim | 2000 | 1544 | 456 | 373 | 845 | 1358 | 1498 |
| distorted | photon | 2000 | 1817 | 183 | 479 | 1012 | 1574 | 1740 |
| acma | gridpin | 1000 | 932 | 68 | 430 | 658 | 857 | 911 |
| acma | nominatim | 1000 | 230 | 770 | 74 | 130 | 205 | 226 |
| acma | photon | 1000 | 404 | 596 | 168 | 252 | 354 | 376 |

| Set | Comparator | GridPin only | Comparator only | Both | Neither | p | Reading |
|---|---|---:|---:|---:|---:|---:|---|
| clean | Nominatim | 563 | 127 | 937 | 373 | 2.40014e-66 | GridPin ahead |
| clean | Photon | 553 | 135 | 947 | 365 | 6.8385e-61 | GridPin ahead |
| distorted | Nominatim | 738 | 91 | 754 | 417 | 1.08071e-126 | GridPin ahead |
| distorted | Photon | 615 | 135 | 877 | 373 | 5.71364e-74 | GridPin ahead |
| acma | Nominatim | 540 | 12 | 118 | 330 | 2.05511e-142 | GridPin ahead |
| acma | Photon | 441 | 35 | 217 | 307 | 1.55601e-90 | GridPin ahead |

Services: public Nominatim and public Photon; answers collected 2026-11-04 and retained. Top-1 great-circle distance to
independent truth, computed with `wikidata4_regress.py`. Radio sites: Based on Australian Communications and Media
Authority information.

<details><summary>Data fingerprints (SHA-256)</summary>

| Artifact | SHA-256 |
|---|---|
| Measured GridPin binary | `f2dea2f0b9dc124b9342cb7c404b932c607af92bd77059ce9addb582d9bc5926` |
| Release sheet `australia.bin` (v0.8.0) | `6f19f33536894ff5b0d3444cee6d47ccc57a77a5170a3bcb01244114e68b3867` |
| Sheet `au.bin` before the metadata-only repack, measured | `04690a2eacdafe663b949dc96f892d1694bf6942331a7199bd7119b9c1679bc5` |
| clean queries | `a53ec286352c31ef1c388cf6a2900df6d437dc66f6a51a3e9a0c2a5f1b852442` |
| clean truth | `03f85ca677c3d25e17fcff678b6696f82275fa53911b4f16ee409183d1c43177` |
| clean gridpin raw | `df952f11d23130723ebd9eb8879178646466e483a9070873fdc4860c8f33e8de` |
| clean nominatim raw | `699eca36b02113a4d9b0ff605a151bc47d85f39acbbd24b168c2f1f96e6919a1` |
| clean photon raw | `0bdd3eacf37d33754249286781ca77be642ff0cf0e1252e6620dc84954dd1b67` |
| distorted queries | `537e49c3c3d703c3bf9dc3d90f52812d20ac14698b247897cdb97eb943e2d547` |
| distorted truth | `afbdc21f7839dfb0d51f8e1286e4c27575e4a05f74263ebf2aaf81800bdc2b14` |
| distorted gridpin raw | `77d538afd1f1f0fa0845b2a7899f7944f34f1486f36b4f6ab44fcdfd760111b7` |
| distorted nominatim raw | `9c7029b36fc0824594a6da82291a6d064fc8138c0adb5dfb8e2da48a6a28abf4` |
| distorted photon raw | `3cef0ab1bd427fdda2d221a903b1bbb77df71c67a6a7cc89987ddf68b9db059b` |
| acma queries | `53a548e8b634b90d2b5106797abf38560eddcc29adf62f7e348a7f5727813217` |
| acma truth | `4719fda3201e3aa19376c94c6be04bb120e66b8bbe5a684aa4a21f9444980f45` |
| acma gridpin raw | `05713c08f281fd6fa1f4edbc98cb32331fca15b3f035f5b42995e93d03488d41` |
| acma nominatim raw | `a59b4aa49ae4be830ae84033de7720766e9219e1ddbfb2b269e02eb2b8cf5112` |
| acma photon raw | `5509c3bd20eece12d3331111332ee72c2151ffc447abc5138107b84b736009b8` |

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
