# Geocoding: open a sheet once, then geocode or reverse-geocode whole vectors in one call.

.state <- new.env(parent = emptyenv())

#' Open a GridPin sheet
#'
#' Loads a country sheet into memory (the file is memory-mapped, so opening is fast and
#' several R sessions share the pages). Opening the same file again returns the same geocoder.
#'
#' @param sheet A country code or name from [gp_sheets()] (the sheet must be downloaded, see
#'   [gp_download()]) or the path to a `.bin` sheet file.
#' @return An object of class `gridpin_geocoder`.
#' @export
#' @examples
#' g <- gp_open(system.file("extdata", "tiny.bin", package = "gridpin"))
#' gp_geocode("3 Main Street, Springfield", g)
gp_open <- function(sheet) {
  if (inherits(sheet, "gridpin_geocoder")) return(sheet)
  stopifnot(is.character(sheet), length(sheet) == 1L, !is.na(sheet))
  path <- if (file.exists(sheet)) sheet else gp_sheet_path(sheet)
  path <- normalizePath(path, mustWork = TRUE)
  cached <- .state[[path]]
  if (!is.null(cached) && .Call(C_gp_alive, cached$ptr)) return(cached)
  ptr <- .Call(C_gp_new)
  .Call(C_gp_text_call, ptr, 0L, 0L, path)
  g <- structure(list(ptr = ptr, path = path), class = "gridpin_geocoder")
  assign(path, g, envir = .state)
  g
}

#' @export
print.gridpin_geocoder <- function(x, ...) {
  cat("<gridpin_geocoder>", x$path, "\n")
  invisible(x)
}

#' Close a geocoder and unmap its sheet
#' @param geocoder A geocoder from [gp_open()].
#' @export
gp_close <- function(geocoder) {
  stopifnot(inherits(geocoder, "gridpin_geocoder"))
  .Call(C_gp_close, geocoder$ptr)
  if (exists(geocoder$path, envir = .state, inherits = FALSE)) rm(list = geocoder$path, envir = .state)
  invisible(NULL)
}

.gp_columns <- c("query", "rank", "lat", "lon", "precision", "confidence", "weak",
                 "housenumber", "street", "postcode", "commune", "flags")

# One data frame row per hit; a query without hits keeps one row with rank NA.
.gp_table <- function(input, rows, extra = NULL) {
  hits <- lapply(rows, function(r) if (is.na(r)) list() else jsonlite::parse_json(r))
  n <- lengths(hits)
  idx <- rep(seq_along(hits), pmax(n, 1L))
  rank <- unlist(lapply(n, function(m) if (m == 0L) NA_integer_ else seq_len(m)))
  flat <- unlist(lapply(hits, function(h) if (length(h)) h else list(NULL)), recursive = FALSE)
  field <- function(name, na) vapply(flat, function(h) {
    v <- h[[name]]
    if (is.null(v)) na else v
  }, na)
  out <- data.frame(query = input[idx], rank = rank, stringsAsFactors = FALSE)
  out$lat <- field("lat", NA_real_)
  out$lon <- field("lon", NA_real_)
  out$precision <- field("precision", NA_character_)
  out$confidence <- field("confidence", NA_real_)
  out$weak <- field("weak", NA)
  out$housenumber <- field("housenumber", NA_character_)
  out$street <- field("street", NA_character_)
  out$postcode <- field("postcode", NA_character_)
  out$commune <- field("commune", NA_character_)
  out$flags <- vapply(flat, function(h) paste(unlist(h$flags), collapse = ";"), "")
  for (name in extra) out[[name]] <- field(name, NA_real_)
  out
}

.gp_k <- function(k) {
  if (!is.numeric(k) || length(k) != 1L || is.na(k) || k < 1 || k != round(k))
    stop("k must be a whole number of at least 1", call. = FALSE)
  as.integer(k)
}

.gp_sf <- function(df, sf) {
  if (!isTRUE(sf)) return(df)
  if (!requireNamespace("sf", quietly = TRUE))
    stop("sf = TRUE needs the sf package: install.packages('sf')", call. = FALSE)
  sf::st_as_sf(df, coords = c("lon", "lat"), crs = 4326, na.fail = FALSE, remove = FALSE)
}

#' Geocode addresses offline
#'
#' Every address is matched on your machine against the sheet; nothing is sent anywhere.
#'
#' @param x A character vector of addresses. `NA` gives a row without coordinates.
#' @param sheet A geocoder from [gp_open()], a country code, or a path to a sheet.
#' @param k How many candidates to return per address. With `k = 1` the result has one row per
#'   address, in input order; with more, one row per candidate and `rank` numbers them.
#' @param sf Return an `sf` object with WGS84 points (needs the sf package).
#' @return A data frame with `query`, `rank`, `lat`, `lon`, `precision` (`house`, `interp`,
#'   `near`, `street`, `city`), `confidence`, `weak` (`TRUE` when the answer is too uncertain to
#'   use without a check), the matched `housenumber`, `street`, `postcode`, `commune`, and
#'   `flags`. Addresses that are not found keep their row with `NA` coordinates.
#' @export
#' @examples
#' g <- gp_open(system.file("extdata", "tiny.bin", package = "gridpin"))
#' gp_geocode(c("3 Main Street, Springfield", "Nowhere 99"), g)
gp_geocode <- function(x, sheet, k = 1, sf = FALSE) {
  stopifnot(is.character(x))
  g <- gp_open(sheet)
  rows <- .Call(C_gp_text_call, g$ptr, 3L, .gp_k(k), enc2utf8(x))
  .gp_sf(.gp_table(x, rows), sf)
}

#' Add coordinates to a data frame column of addresses
#'
#' @param df A data frame.
#' @param column Name of the column with the address text.
#' @inheritParams gp_geocode
#' @return `df` with the columns of [gp_geocode()] prefixed `gp_` (except `query` and `rank`).
#' @export
gp_geocode_df <- function(df, column, sheet, sf = FALSE) {
  stopifnot(is.data.frame(df), column %in% names(df))
  res <- gp_geocode(as.character(df[[column]]), sheet, k = 1)
  res$query <- NULL
  res$rank <- NULL
  names(res) <- paste0("gp_", names(res))
  out <- cbind(df, res)
  if (isTRUE(sf)) {
    out$lat <- out$gp_lat
    out$lon <- out$gp_lon
    out <- .gp_sf(out, TRUE)
    out$lat <- NULL
    out$lon <- NULL
  }
  out
}

#' Reverse geocode: the nearest indexed address to each point
#'
#' @param lat,lon Numeric vectors of WGS84 coordinates of the same length.
#' @inheritParams gp_geocode
#' @return A data frame like [gp_geocode()] with `distance_m`, the distance from the point to the
#'   answer. Streets are indexed by their centre, so check `distance_m` before treating the answer
#'   as the building at the point.
#' @export
gp_reverse <- function(lat, lon, sheet, k = 1) {
  stopifnot(is.numeric(lat), is.numeric(lon), length(lat) == length(lon))
  g <- gp_open(sheet)
  rows <- .Call(C_gp_reverse_call, g$ptr, .gp_k(k), as.double(lat), as.double(lon))
  out <- .gp_table(sprintf("%.7f, %.7f", lat, lon), rows, extra = "distance_m")
  names(out)[1] <- "point"
  out
}

#' Versions, for the methods section of your study
#'
#' @param sheet Optional: a geocoder or sheet, to report its file and size.
#' @return A named list: package version, C interface version, sheet release, and the sheet file.
#' @export
gp_version <- function(sheet = NULL) {
  out <- list(package = as.character(utils::packageVersion("gridpin")),
              abi = .Call(C_gp_abi_version), release = .gp_release)
  if (!is.null(sheet)) {
    g <- gp_open(sheet)
    out$sheet <- g$path
    out$sheet_bytes <- file.size(g$path)
  }
  out
}
