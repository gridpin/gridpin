# Sheets: where they live, whether they are here, and a verified download.

#' Directory for downloaded sheets
#'
#' `GRIDPIN_HOME` if set, otherwise the user cache directory for R packages
#' ([tools::R_user_dir()]). Sheets already downloaded by the Python package `gridpin-mcp`
#' (`~/.cache/gridpin`) are found and used without a second download.
#' @export
gp_home <- function() {
  explicit <- Sys.getenv("GRIDPIN_HOME")
  if (nzchar(explicit)) return(path.expand(explicit))
  tools::R_user_dir("gridpin", "cache")
}

.gp_python_home <- function() {
  cache <- Sys.getenv("XDG_CACHE_HOME")
  file.path(if (nzchar(cache)) path.expand(cache) else path.expand("~/.cache"), "gridpin")
}

.gp_resolve <- function(country) {
  stopifnot(is.character(country), length(country) == 1L, !is.na(country))
  key <- tolower(trimws(country))
  hit <- which(.gp_sheets$code == key | tolower(.gp_sheets$name) == key)
  if (!length(hit)) {
    stop(sprintf("unknown country '%s'; available: %s", country,
                 paste(sprintf("%s (%s)", .gp_sheets$code, .gp_sheets$name), collapse = ", ")),
         call. = FALSE)
  }
  as.list(.gp_sheets[hit[1], ])
}

# Every place a sheet may already be; a file counts only with the pinned size.
.gp_candidates <- function(sheet) {
  c(file.path(gp_home(), .gp_release, sheet$file),
    file.path(.gp_python_home(), .gp_release, sheet$file))
}

.gp_find <- function(sheet) {
  for (path in .gp_candidates(sheet)) {
    if (file.exists(path) && isTRUE(file.size(path) == sheet$size)) return(path)
  }
  NA_character_
}

#' Available sheets and whether they are downloaded
#' @return A data frame: `code`, `country`, `size_mb`, `downloaded`, `path`.
#' @export
#' @examples
#' gp_sheets()
gp_sheets <- function() {
  paths <- vapply(seq_len(nrow(.gp_sheets)), function(i) .gp_find(as.list(.gp_sheets[i, ])), "")
  data.frame(code = .gp_sheets$code, country = .gp_sheets$name,
             size_mb = round(.gp_sheets$size / 1e6), downloaded = !is.na(paths), path = paths,
             stringsAsFactors = FALSE)
}

#' Path of a downloaded sheet
#' @param country A country code or name from [gp_sheets()].
#' @export
gp_sheet_path <- function(country) {
  sheet <- .gp_resolve(country)
  path <- .gp_find(sheet)
  if (is.na(path)) {
    stop(sprintf("the %s sheet is not downloaded (%.0f MB); run gp_download(\"%s\")",
                 sheet$name, sheet$size / 1e6, sheet$code), call. = FALSE)
  }
  path
}

# Replaced in tests; the only function in the package that touches the network.
.gp_fetch <- function(url, destfile) {
  old <- options(timeout = max(3600, getOption("timeout")))
  on.exit(options(old))
  utils::download.file(url, destfile, mode = "wb", quiet = FALSE,
                       headers = c("User-Agent" = paste0("gridpin-r (", .gp_release, ")")))
}

#' Download a sheet and verify it
#'
#' Fetches the sheet of the GridPin release this package was built for, checks its size and
#' SHA-256 against the values pinned in the package, and only then moves it into [gp_home()].
#' A sheet that is already there is not fetched again. This is the only function that uses the
#' network; set `GRIDPIN_OFFLINE=1` to forbid it.
#'
#' @param country A country code or name from [gp_sheets()].
#' @return The path of the sheet, invisibly.
#' @export
gp_download <- function(country) {
  sheet <- .gp_resolve(country)
  found <- .gp_find(sheet)
  if (!is.na(found)) return(invisible(found))
  if (nzchar(Sys.getenv("GRIDPIN_OFFLINE")) && Sys.getenv("GRIDPIN_OFFLINE") != "0") {
    stop(sprintf("GRIDPIN_OFFLINE is set and the %s sheet is not downloaded", sheet$name), call. = FALSE)
  }
  target <- file.path(gp_home(), .gp_release, sheet$file)
  dir.create(dirname(target), recursive = TRUE, showWarnings = FALSE)
  part <- tempfile(paste0(sheet$file, "."), tmpdir = dirname(target), fileext = ".part")
  on.exit(unlink(part), add = TRUE)
  url <- paste(.gp_base_url, .gp_release, sheet$file, sep = "/")
  status <- tryCatch(.gp_fetch(url, part), error = function(e) {
    stop(sprintf("could not download %s: %s", url, conditionMessage(e)), call. = FALSE)
  })
  if (!identical(as.integer(status), 0L)) stop(sprintf("download of %s failed (status %s)", url, status), call. = FALSE)
  got <- file.size(part)
  if (!isTRUE(got == sheet$size)) {
    stop(sprintf("%s: got %s bytes, expected %s; retry", url, got, sheet$size), call. = FALSE)
  }
  if (!identical(gp_sha256(part), sheet$sha256)) {
    stop(sprintf("%s: SHA-256 does not match the signed release; not used", url), call. = FALSE)
  }
  if (!file.rename(part, target)) stop(sprintf("could not move the sheet to %s", target), call. = FALSE)
  invisible(target)
}

#' SHA-256 of a file
#'
#' Uses [tools::sha256sum()] (R 4.5 and newer) or, on older R, the digest package.
#' @param path Path to a file.
#' @return Lower-case hexadecimal digest.
#' @export
gp_sha256 <- function(path) {
  stopifnot(is.character(path), length(path) == 1L, file.exists(path))
  tools_ns <- asNamespace("tools")
  if (exists("sha256sum", envir = tools_ns, inherits = FALSE)) {
    return(unname(get("sha256sum", envir = tools_ns)(path)))
  }
  if (requireNamespace("digest", quietly = TRUE)) {
    return(digest::digest(path, algo = "sha256", file = TRUE))
  }
  stop("checking a download needs R 4.5 or newer, or the digest package: install.packages('digest')",
       call. = FALSE)
}
