// CSV of the /ui page of `gridpin serve`: read and write RFC 4180 text (quotes, doubled quotes, delimiters and line
// breaks inside a quoted cell), in the browser and in Node alike. No dependencies. The delimiter is the comma, the
// semicolon (Excel saves "CSV" with ";" where the decimal mark is a comma: Germany, France) or the tab, found in the
// header row; the file is written back with the delimiter it came with.

/// The delimiter of a CSV: the one of ",", ";" and tab that occurs most often outside quotes in the first row; a tie or
/// none is the comma. The header row names columns, so it holds the delimiter and rarely a comma of its own.
export function sniffDelimiter(text) {
  const counts = { ",": 0, ";": 0, "\t": 0 };
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === '"') quoted = !quoted;
    else if (!quoted && (c === "\n" || c === "\r")) break;
    else if (!quoted && c in counts) counts[c]++;
  }
  let best = ",";
  for (const d of [";", "\t"]) if (counts[d] > counts[best]) best = d;
  return best;
}

export function parseCsv(text, delimiter = ",") {
  const rows = [];
  let row = [];
  let cell = "";
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quoted) {
      if (c === '"') {
        if (text[i + 1] === '"') {
          cell += '"';
          i++;
        } else {
          quoted = false;
        }
      } else {
        cell += c;
      }
    } else if (c === '"') {
      quoted = true;
    } else if (c === delimiter) {
      row.push(cell);
      cell = "";
    } else if (c === "\n" || c === "\r") {
      if (c === "\r" && text[i + 1] === "\n") i++;
      row.push(cell);
      rows.push(row);
      row = [];
      cell = "";
    } else {
      cell += c;
    }
  }
  if (cell !== "" || row.length) {
    row.push(cell);
    rows.push(row);
  }
  return rows.filter((r) => !(r.length === 1 && r[0] === ""));
}

export function toCsv(rows, delimiter = ",") {
  const esc = (v) => {
    const s = v == null ? "" : String(v);
    return /["\r\n]/.test(s) || s.includes(delimiter) ? `"${s.replace(/"/g, '""')}"` : s;
  };
  return rows.map((r) => r.map(esc).join(delimiter)).join("\n") + "\n";
}

/// The text of a file read as UTF-8, or null when it is not UTF-8 (Excel's plain "CSV" is Windows-1252: "Große" would
/// come back as "Gro\uFFFDe", silently, in the user's own columns too). A UTF-8 byte order mark is dropped.
export function utf8Text(bytes) {
  try {
    return new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes);
  } catch {
    return null;
  }
}

/// The level counted on the page for one row: a weak answer is counted apart from the level it claims.
export function levelOf(hit) {
  if (!hit) return "none";
  return hit.weak ? `${hit.match_level} (weak)` : hit.match_level;
}

/// The longest query the server reads (`MAX_QUERY_BYTES` in query.rs); a longer cell is not sent, so one note in a
/// cell cannot turn a whole chunk into "414" (revision 0.9.0).
export const MAX_QUERY_BYTES = 1024;

/// The query sent for a cell: the cell, or "" with the reason when it cannot be asked.
export function queryOf(cell) {
  const text = cell || "";
  if (new TextEncoder().encode(text).length > MAX_QUERY_BYTES) return { query: "", skipped: "too_long" };
  return { query: text, skipped: null };
}

// gp_weak and gp_confidence tell a weak answer from a house: "Marienplatz 1, 80331 München" on the German sheet (Bavaria
// is not in it) is a house in Schwerin with match_level PointAddress, confidence 0.2 and nothing else in the old columns said so
export const ANSWER_COLUMNS = ["gp_lat", "gp_lon", "gp_match_level", "gp_weak", "gp_confidence", "gp_street", "gp_housenumber", "gp_commune", "gp_postcode"];

/// The cells the page adds for one answer. No answer: empty cells, and `gp_match_level` says why when the row was not
/// asked or its chunk failed ("too_long", "error").
export function answerCells(hit, level) {
  if (!hit) return ANSWER_COLUMNS.map((c) => (c === "gp_match_level" && level && level !== "none" ? level : ""));
  return [hit.lat, hit.lon, hit.match_level, hit.weak ? "true" : "false", hit.confidence, hit.street, hit.housenumber ?? "", hit.commune, hit.postcode];
}
