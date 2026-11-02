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
}

export class Geocoder {
  constructor(sheet: string, options?: { poi?: string });
  geocode(query: string, options?: { k?: number }): Hit[];
  geocodeMany(queries: Iterable<string | null | undefined>, options?: { k?: number }): Hit[][];
  reverse(lat: number, lon: number, options?: { k?: number }): Hit[];
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
