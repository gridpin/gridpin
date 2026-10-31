test_that("a known address is found as a house with its coordinates", {
  g <- gp_open(tiny())
  r <- gp_geocode("3 Main Street, Springfield", g)
  expect_equal(nrow(r), 1L)
  expect_equal(r$precision, "house")
  expect_equal(r$housenumber, "3")
  expect_equal(r$street, "Main Street")
  expect_equal(c(r$lat, r$lon), c(40.7502, -73.9902), tolerance = 1e-7)
  expect_false(r$weak)
  expect_equal(r$rank, 1L)
})

test_that("k = 1 keeps one row per input, in order, NA and misses included", {
  r <- gp_geocode(c("10 Oak Avenue, Springfield", NA, "zzzz qqqq", "1 Main Street, Springfield"), tiny())
  expect_equal(nrow(r), 4L)
  expect_identical(r$query, c("10 Oak Avenue, Springfield", NA, "zzzz qqqq", "1 Main Street, Springfield"))
  expect_equal(r$street[c(1, 4)], c("Oak Avenue", "Main Street"))
  expect_true(all(is.na(r$lat[2:3])))
  expect_true(all(is.na(r$rank[2:3])))
})

test_that("k > 1 gives ranked candidates in a long table", {
  r <- gp_geocode(c("1 Main Street", "3 Main Street, Springfield"), tiny(), k = 3)
  homonyms <- r[r$query == "1 Main Street", ]
  expect_equal(homonyms$rank, 1:2)
  expect_setequal(homonyms$commune, c("Springfield", "Shelbyville"))
  expect_equal(sum(r$query == "3 Main Street, Springfield"), 1L)
})

test_that("weak marks answers that are not a door-level match", {
  r <- gp_geocode("Springfield", tiny())
  expect_equal(r$precision, "city")
  expect_true(r$weak)
})

test_that("k must be a whole number of at least 1", {
  expect_error(gp_geocode("x", tiny(), k = 0), "at least 1")
  expect_error(gp_geocode("x", tiny(), k = 1.5), "whole number")
})

test_that("reverse geocoding returns the nearest house and its distance", {
  r <- gp_reverse(40.7504, -73.9904, tiny())
  expect_equal(r$housenumber, "5")
  expect_lt(r$distance_m, 1)
  expect_true(is.na(gp_reverse(NA_real_, 1, tiny())$lat))
})

test_that("opening the same sheet twice returns the same geocoder, and a closed one fails cleanly", {
  a <- gp_open(tiny())
  expect_identical(gp_open(tiny())$ptr, a$ptr)
  gp_close(a)
  expect_error(gp_geocode("x", a), "closed")
  expect_equal(gp_geocode("3 Main Street, Springfield", tiny())$housenumber, "3")
})

test_that("a missing sheet file is a clear error", {
  expect_error(gp_open(file.path(tempdir(), "no-such-sheet.bin")), "not downloaded|unknown country|cannot")
})

test_that("gp_geocode_df adds prefixed columns and keeps the data frame", {
  df <- data.frame(id = 1:2, address = c("3 Main Street, Springfield", "nowhere"))
  out <- gp_geocode_df(df, "address", tiny())
  expect_equal(out$id, 1:2)
  expect_equal(out$gp_housenumber[1], "3")
  expect_true(is.na(out$gp_lat[2]))
})

test_that("sf = TRUE gives WGS84 points", {
  skip_if_not_installed("sf")
  r <- gp_geocode(c("3 Main Street, Springfield", "nowhere"), tiny(), sf = TRUE)
  expect_s3_class(r, "sf")
  expect_equal(sf::st_crs(r)$epsg, 4326L)
  expect_equal(unname(sf::st_coordinates(r)[1, ]), c(-73.9902, 40.7502), tolerance = 1e-7)
})

test_that("versions name the package, the interface and the release", {
  v <- gp_version(tiny())
  expect_equal(v$abi, 1L)
  expect_match(v$release, "^v[0-9]")
  expect_equal(v$sheet_bytes, file.size(tiny()))
})
