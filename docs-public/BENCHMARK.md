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

**What it shows.** Distorted addresses: GridPin 33.84%, Photon 29.56%, Nominatim 20.82% — GridPin ahead of
both, beyond chance, though by less than in Germany. Andalusia schools: Photon 63.50% leads; GridPin 59.20% and
Nominatim 60.80% are level (not proof of parity).

| Set | Engine | N | Found | Empty | ≤50 m | ≤150 m | ≤1,000 m | ≤5,000 m |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| andalusia | gridpin | 2000 | 1802 | 198 | 887 | 1184 | 1490 | 1638 |
| andalusia | nominatim | 2000 | 1666 | 334 | 700 | 1216 | 1568 | 1613 |
| andalusia | photon | 2000 | 1938 | 62 | 726 | 1270 | 1760 | 1894 |
| distorted | gridpin | 1590 | 1048 | 542 | 385 | 538 | 738 | 958 |
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
| clean | gridpin | 2376 | 1551 | 825 | 270 | 551 | 972 | 1251 |
| clean | nominatim | 2376 | 1009 | 1367 | 172 | 412 | 783 | 884 |
| clean | photon | 2376 | 1494 | 882 | 311 | 607 | 1114 | 1271 |
| distorted | gridpin | 2376 | 1372 | 1004 | 229 | 492 | 846 | 1093 |
| distorted | nominatim | 2376 | 514 | 1862 | 100 | 229 | 413 | 461 |
| distorted | photon | 2376 | 1206 | 1170 | 241 | 471 | 879 | 996 |
| cnes | gridpin | 1000 | 752 | 248 | 270 | 368 | 604 | 707 |
| cnes | nominatim | 1000 | 512 | 488 | 122 | 207 | 449 | 504 |
| cnes | photon | 1000 | 802 | 198 | 160 | 282 | 643 | 756 |

Exact paired two-sided McNemar tests at ≤150 m:

| Set | Comparator | GridPin only | Comparator only | Both | Neither | p | Reading |
|---|---|---:|---:|---:|---:|---:|---|
| distorted | Nominatim | 373 | 110 | 119 | 1,774 | 1.72106e-34 | GridPin ahead |
| distorted | Photon | 277 | 256 | 215 | 1,628 | 0.386346 | level — not proof of parity |
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

**What it shows.** Distorted addresses: GridPin 32.72% leads Photon 28.45% and Nominatim 24.32%, beyond chance.
Historic-places register: GridPin 64.00% leads Photon 38.90% and Nominatim 52.30%, beyond chance. Clean addresses:
on clean US addresses Nominatim and Photon both lead (42.5% and 41.3% vs 39.1%). One nationwide sheet with point
addresses in 35 states and Census address ranges elsewhere; range answers are interpolated.

| Set | Engine | N | Found | Empty | ≤50 m | ≤150 m | ≤1,000 m | ≤5,000 m |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| clean | gridpin | 3655 | 3469 | 186 | 847 | 1430 | 2096 | 2291 |
| clean | nominatim | 3655 | 3222 | 433 | 991 | 1553 | 2192 | 2338 |
| clean | photon | 3655 | 3490 | 165 | 1010 | 1510 | 2224 | 2482 |
| distorted | gridpin | 3655 | 3061 | 594 | 721 | 1204 | 1699 | 1867 |
| distorted | nominatim | 3655 | 1796 | 1859 | 571 | 889 | 1330 | 1427 |
| distorted | photon | 3655 | 2864 | 791 | 698 | 1040 | 1601 | 1804 |
| nrhp | gridpin | 1000 | 895 | 105 | 468 | 641 | 763 | 825 |
| nrhp | nominatim | 1000 | 810 | 190 | 359 | 523 | 707 | 774 |
| nrhp | photon | 1000 | 880 | 120 | 271 | 389 | 642 | 783 |

Exact paired two-sided McNemar tests at ≤150 m:

| Set | Comparator | GridPin only | Comparator only | Both | Neither | p | Reading |
|---|---|---:|---:|---:|---:|---:|---|
| clean | Nominatim | 347 | 481 | 1,072 | 1,755 | 3.62795e-06 | Nominatim ahead |
| clean | Photon | 473 | 564 | 946 | 1,672 | 0.00516835 | Photon ahead |
| distorted | Nominatim | 657 | 342 | 547 | 2,109 | 1.26392e-23 | GridPin ahead |
| distorted | Photon | 616 | 452 | 588 | 1,999 | 5.82017e-07 | GridPin ahead |
| nrhp | Nominatim | 176 | 58 | 465 | 301 | 5.22407e-15 | GridPin ahead |
| nrhp | Photon | 309 | 57 | 332 | 302 | 5.28401e-43 | GridPin ahead |

Services: public Nominatim and public Photon, answers collected 2026-10-20 and retained; no new competitor
requests were made for the GridPin build measured on 2026-10-23. Top-1 great-circle distance to independent
truth, computed with `wikidata4_regress.py`.

<details><summary>Data fingerprints (SHA-256)</summary>

| Artifact | SHA-256 |
|---|---|
| Measured GridPin binary | `648482624e93a2f1f5b44f1bf2f7f1a5c00e3dcfdfeba956905b886fcb12e0ce` |
| Measured sheet `us.bin` | `8fbdf76105057eab264dd40d37e080eeb506c09f23562718b9173035cae1a258` |
| Release sheet `usa.bin` (v0.5.0; metadata-only repack of the measured sheet, 2026-10-23) | `4f3422f4d95939aefbcf454ceebc9a8dd16f78f2a13c5e6660028ddfad712cc0` |
| clean queries | `f5e07a251a447508482fa1b9ead94c4c662d0f6c8f91a28e9eab5b1cf054549e` |
| clean truth | `a7ba4d3492c63372af5af98f6c791e1fa0eb83e61845f528596a944d7bae3dd9` |
| clean gridpin raw | `f92faf60837ac7c85edf3cdb8c75ca38b42c48d84f6455744566a802c0935f1a` |
| clean nominatim raw | `b88906d62ac4ba2104ad335e540a1d1d328edcdb9e0eeb924d8bfd4164d9b719` |
| clean photon raw | `e2d2c6cfdd065177895c7a9829c1e147f6b34115d06cfc869dcf26551dfda642` |
| distorted queries | `4159ca5da2e1945327268e89d5a7a7470c07b1e31811478887ce57ab4e14fc01` |
| distorted truth | `5a3e433784eb574b016e72704d4add23013f322cde24e78b5ea5b7e3acc26932` |
| distorted gridpin raw | `449fedf20900ee3309c306a2fb756cec1d1fd68ed3741eef7bbdcb70e4f5f111` |
| distorted nominatim raw | `1aa04709b79dea7863559cdece240927926f8d8ea38fb9e6dc3078431bab557e` |
| distorted photon raw | `e5d9d74c3c0f869cf22688c77945259c0725add1d3f4bb2ee6c318192b224c2f` |
| nrhp queries | `661e364034b7efa66323d7f050840c14ec3013b628ae4b4d924a9e6fff901ce1` |
| nrhp truth | `d8db31e9c8b7286005589436f41177fca5c3fcfc2da829582cd1cddd88b5ec50` |
| nrhp gridpin raw | `c108b09f21bb083e2af8e120b08c0d081d78f4d2de8d21dcd9499b019cea04c3` |
| nrhp nominatim raw | `45fe8cf55be0df4578061d3c793126fe4cff0d4a329365b7104923560dcdcead` |
| nrhp photon raw | `e4b3c98476ac16c529117920d3d261f04e42e32ecbb4c82f6b79a1f59266659c` |

</details>

## Canada · 2026-10-30 · 3 sets

**What we tested.** Three retained sets with known coordinates: clean Wikidata and mechanically distorted Wikidata
(2,785 rows each; three rows outside Canada excluded equally for all three engines) and 1,000 radio-station sites
from the ISED Technical and Administrative Frequency List, as recorded. These sets are not observed human search
traffic.

**What it shows.** Distorted addresses: GridPin 55.91% leads Photon 45.21% and Nominatim 32.68%, beyond chance.
Radio-station sites: GridPin 39.30% and Photon 39.30% are level (not proof of parity), both ahead of Nominatim
25.20%. Clean addresses: on clean Canadian addresses Photon leads (59.1% vs 57.5%; Nominatim 57.2%); the set cannot tell the three apart beyond chance. One
nationwide sheet with point addresses in all provinces and territories; answers carry the Canadian postal code.

| Set | Engine | N | Found | Empty | ≤50 m | ≤150 m | ≤1,000 m | ≤5,000 m |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| clean | gridpin | 2785 | 2697 | 88 | 1060 | 1601 | 1948 | 2044 |
| clean | nominatim | 2785 | 2587 | 198 | 1011 | 1593 | 1912 | 2035 |
| clean | photon | 2785 | 2751 | 34 | 1038 | 1646 | 1988 | 2110 |
| distorted | gridpin | 2785 | 2657 | 128 | 1032 | 1557 | 1896 | 1995 |
| distorted | nominatim | 2785 | 1493 | 1292 | 602 | 910 | 1109 | 1185 |
| distorted | photon | 2785 | 2453 | 332 | 827 | 1259 | 1535 | 1652 |
| tafl | gridpin | 1000 | 808 | 192 | 184 | 393 | 620 | 697 |
| tafl | nominatim | 1000 | 605 | 395 | 131 | 252 | 425 | 537 |
| tafl | photon | 1000 | 806 | 194 | 204 | 393 | 607 | 696 |

Exact paired two-sided McNemar tests at ≤150 m:

| Set | Comparator | GridPin only | Comparator only | Both | Neither | p | Reading |
|---|---|---:|---:|---:|---:|---:|---|
| clean | Nominatim | 392 | 384 | 1,209 | 800 | 0.801613 | level |
| clean | Photon | 349 | 394 | 1,252 | 790 | 0.10642 | level |
| distorted | Nominatim | 866 | 219 | 691 | 1,009 | 1.91702e-91 | GridPin ahead |
| distorted | Photon | 625 | 327 | 932 | 901 | 2.8455e-22 | GridPin ahead |
| tafl | Nominatim | 200 | 59 | 193 | 548 | 4.10126e-19 | GridPin ahead |
| tafl | Photon | 120 | 120 | 273 | 487 | 1 | level |

Services: public Nominatim and public Photon, answers collected 2026-10-28 and retained; no new competitor
requests were made for the GridPin build measured on 2026-10-30. Top-1 great-circle distance to independent
truth, computed with `wikidata4_regress.py`.

<details><summary>Data fingerprints (SHA-256)</summary>

| Artifact | SHA-256 |
|---|---|
| Measured GridPin binary | `2a894e8efca7377e81eb828384a35636b6634e234ef39bf6922eab53cd3280ea` |
| Measured sheet `ca.bin` | `fcd4d9a9d1c5a1540722a66e7cad470ec515525f333df5f9ddfddea337449b44` |
| Release sheet `canada.bin` (v0.6.0; metadata-only repack of the measured sheet, 2026-10-30) | `234515843c54b6c4feb93ab83c7b023a3e14beec8357c6d570eeed0f66f35400` |
| clean queries | `401609e84a568e00bd430f2491431d414635988c54b6191df5242d54b7a8be40` |
| clean truth | `3ff8483c360b597a95b2da76cc29d22311f99bc6b6c2e550b8ee06f92c1e0153` |
| clean gridpin raw | `af79fb1cdcd2b1b2ae3817a9c26d3bc5ac8b539e16db8ecd033386c5111429a2` |
| clean nominatim raw | `222b8a48d3e87f406e0e9b1764e6c79339c7da358d673fba6a111561f8c39477` |
| clean photon raw | `8609334d60f7cc673d83dde7898f59bccdd46551dea57609439cf9306ffa5568` |
| distorted queries | `e705a0f57d7423c49c7f66fd4fa06ea5083c744fedae226be46c7ece60557434` |
| distorted truth | `c06654e2f12da2f88fd1ea6e0e3e3f9c5a485c9f5e6922ff8a0469c0d01e41bb` |
| distorted gridpin raw | `fffec86d74e4257bfb2a1b743572db570d0e30bec933363547e131e3f9e1aa0d` |
| distorted nominatim raw | `ea8dc780de77735515532febb8cfd0cc4c110a085341d844e9958fd914766afb` |
| distorted photon raw | `eb8517e1452087ea4b622d4b6a558fc4edfb0732731a912f27202ba88af54015` |
| tafl queries | `f913520c7f93508606b9974a7a9fb6bd40209264ff4a570f111aa8a5c5127262` |
| tafl truth | `b0e00a314851314ef31720e380025a1dff06e6df9bd4c6aaa1dcbb188d78b605` |
| tafl gridpin raw | `8c463c509156d6eb388d602597acdc50d97c21a34bb05b446153f8acd1c2a12a` |
| tafl nominatim raw | `33100eeed662f19d33b24981f371993d823cd9bb60faa2befc4ebd374c7bed1c` |
| tafl photon raw | `2ba010a9bfd13122223d09c2c79d5fd074955f47c9ed686e4ea4c76fdf1e35f3` |

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
