export interface Hit {
  lat: number;
  lon: number;
  /** house | interp | near | street | city | approximate (reverse) */
  precision: string;
  score: number;
  confidence: number;
  street: string;
  housenumber?: string;
  commune: string;
  postcode: string;
  flags?: string[];
  region?: string;
  distance_m?: number;
  /** true when the engine itself says the answer needs a check before use */
  weak: boolean;
  /** PointAddress | StreetAddress | StreetAddressExt | StreetName | Postal | Locality | POI */
  match_level: string;
  /** M matched | T tied (the same answers as weak: true) */
  match_status: string;
  /** only with the `with` option: the fields asked for (h3, geohash, placekey_where, plus_code, mgrs, dms, street_point, street_point_m, links, provenance) */
  extras?: Record<string, unknown>;
}

export class Geocoder {
  constructor(sheet: string, options?: { poi?: string });
  /** `with`: extra answer fields as in `gridpin batch --with` ("h3,geohash,placekey,codes,street_point,links,provenance"); each hit gets `extras`. */
  geocode(query: string, options?: { k?: number; with?: string }): Hit[];
  geocodeMany(queries: Iterable<string | null | undefined>, options?: { k?: number; with?: string }): Hit[][];
  reverse(lat: number, lon: number, options?: { k?: number; with?: string }): Hit[];
  close(): void;
}

export interface SheetInfo {
  code: string;
  name: string;
  file: string;
  size: number;
  sha256: string;
  url: string;
  path: string;
  downloaded: boolean;
}

export const sheets: {
  release: string;
  list(): SheetInfo[];
  where(country?: string): string;
  verify(country: string): 'ok' | 'missing' | 'wrong size' | 'wrong sha256';
  fetch(country: string, options?: { offline?: boolean }): Promise<string>;
};

export function abiVersion(): number;
