# gridpin for R

Offline geocoding in R: addresses to coordinates and back, on your machine, with one file per
country. No server, no API key, no request per address. The address text never leaves your
computer; the only network access is the one-time download of a country file, which you can do
in advance.

```r
library(gridpin)

gp_sheets()                     # available countries, file sizes, what is already downloaded
gp_download("us")               # once: fetched and checked against the release checksums

addresses <- c("1600 Pennsylvania Ave NW, Washington, DC 20500",
               "233 S Wacker Dr, Chicago, IL 60606")
gp_geocode(addresses, "us")
gp_geocode(addresses, "us", sf = TRUE)           # an sf object with WGS84 points
gp_geocode_df(my_data, "address", "us")          # adds gp_lat, gp_lon, gp_precision, ...
gp_reverse(38.8977, -77.0365, "us")              # nearest address and its distance
```

Every answer says how good it is: `precision` is `house`, `interp`, `near`, `street` or `city`,
`confidence` is a score from 0 to 1, and `weak` is `TRUE` when the answer needs a check before
you use it (a town-level answer, a low score, or a fuzzy street match). Addresses that are not
found keep their row with `NA` coordinates instead of a guess.

For the methods section of a study, `gp_version("us")` reports the package, the interface and
the sheet release.

## Install

```r
install.packages("gridpin", repos = c("https://gridpin.r-universe.dev", "https://cloud.r-project.org"))
```

R-universe serves binary packages for Windows and macOS, which need no Rust. Installing from
source (Linux, or any system without a binary) builds the Rust code, so it needs `cargo` and `rustc`
1.87 or newer (`apt-get install cargo rustc`, `dnf install cargo`, or <https://rustup.rs>).

Covered countries and file sizes: <https://gridpin.dev/guides#countries>. Quality figures:
<https://gridpin.dev/bench>.

Sheets are stored in `tools::R_user_dir("gridpin", "cache")`, or in `GRIDPIN_HOME` if set. A
sheet already downloaded by the Python package `gridpin-mcp` is found and used as is. Set
`GRIDPIN_OFFLINE=1` to make sure nothing is downloaded.

License: Apache-2.0. The country files carry the licenses of their sources:
<https://github.com/gridpin/gridpin/blob/main/ATTRIBUTIONS.md>.
