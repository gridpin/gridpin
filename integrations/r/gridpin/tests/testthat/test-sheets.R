local_sheet <- function(env = parent.frame()) {
  home <- withr::local_tempdir("gridpin-home-", .local_envir = env)
  withr::local_envvar(GRIDPIN_HOME = home, XDG_CACHE_HOME = file.path(home, "xdg"), GRIDPIN_OFFLINE = NA,
                      .local_envir = env)
  home
}
# A one-sheet registry for the tiny file, with its true size and digest unless told otherwise.
fake_registry <- function(sha = gp_sha256(tiny()), size = file.size(tiny())) {
  data.frame(code = "zz", name = "Testland", file = "tiny.bin", size = size, sha256 = sha,
             stringsAsFactors = FALSE)
}
copying_fetch <- function(calls) function(url, destfile) {
  calls$n <- calls$n + 1L
  file.copy(tiny(), destfile, overwrite = TRUE)
  0L
}

test_that("the registry pins every sheet with a size and a full SHA-256", {
  s <- gridpin:::.gp_sheets
  expect_true(nrow(s) >= 9)
  expect_true(all(grepl("^[0-9a-f]{64}$", s$sha256)))
  expect_true(all(s$size > 1e6))
  expect_equal(gp_sheets()$code, s$code)
})

test_that("a download is verified and moved into place, and not fetched twice", {
  home <- local_sheet()
  calls <- new.env(); calls$n <- 0L
  local_mocked_bindings(.gp_sheets = fake_registry(), .gp_fetch = copying_fetch(calls), .package = "gridpin")
  path <- gp_download("zz")
  expect_equal(path, file.path(home, gridpin:::.gp_release, "tiny.bin"))
  expect_equal(gp_sha256(path), gp_sha256(tiny()))
  expect_equal(gp_download("Testland"), path)
  expect_equal(calls$n, 1L)
  expect_equal(gp_sheet_path("zz"), path)
  expect_equal(list.files(dirname(path)), "tiny.bin")
})

test_that("a download with the wrong SHA-256 is refused and leaves nothing behind", {
  home <- local_sheet()
  calls <- new.env(); calls$n <- 0L
  local_mocked_bindings(.gp_sheets = fake_registry(sha = strrep("0", 64)), .gp_fetch = copying_fetch(calls),
                        .package = "gridpin")
  expect_error(gp_download("zz"), "SHA-256 does not match")
  expect_length(list.files(file.path(home, gridpin:::.gp_release)), 0L)
})

test_that("a download of the wrong size is refused", {
  local_sheet()
  calls <- new.env(); calls$n <- 0L
  local_mocked_bindings(.gp_sheets = fake_registry(size = 5), .gp_fetch = copying_fetch(calls), .package = "gridpin")
  expect_error(gp_download("zz"), "expected 5")
})

test_that("GRIDPIN_OFFLINE forbids the network", {
  local_sheet()
  withr::local_envvar(GRIDPIN_OFFLINE = "1")
  calls <- new.env(); calls$n <- 0L
  local_mocked_bindings(.gp_sheets = fake_registry(), .gp_fetch = copying_fetch(calls), .package = "gridpin")
  expect_error(gp_download("zz"), "GRIDPIN_OFFLINE")
  expect_equal(calls$n, 0L)
})

test_that("a sheet already downloaded by gridpin-mcp (Python) is used without a download", {
  home <- local_sheet()
  py <- file.path(home, "xdg", "gridpin", gridpin:::.gp_release)
  dir.create(py, recursive = TRUE)
  file.copy(tiny(), file.path(py, "tiny.bin"))
  calls <- new.env(); calls$n <- 0L
  local_mocked_bindings(.gp_sheets = fake_registry(), .gp_fetch = copying_fetch(calls), .package = "gridpin")
  expect_equal(normalizePath(gp_download("zz")), normalizePath(file.path(py, "tiny.bin")))
  expect_equal(calls$n, 0L)
})

test_that("an unknown country lists the available ones", {
  expect_error(gp_sheet_path("pl"), "unknown country 'pl'.*us \\(United States\\)")
})
