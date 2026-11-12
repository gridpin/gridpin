// Vercel AI SDK tools over a local GridPin sheet: address to coordinates and back, offline.
//
//   npm install gridpin ai zod
//   import { gridpinTools } from './tool';
//   const result = await generateText({ model, tools: gridpinTools('/path/to/us.bin'), prompt: '...' });
//
// The answers are those of the `gridpin` npm package (the same engine and fields as the CLI and the MCP server).
import { Geocoder } from 'gridpin';
import { tool } from 'ai';
import { z } from 'zod';

export function gridpinTools(sheet: string) {
  const g = new Geocoder(sheet);
  return {
    gridpin_geocode: tool({
      description:
        'Offline geocoding of an address or place: ranked candidates with lat, lon, precision (house, interp, ' +
        'street, city, ...), confidence and weak (true when the answer needs a check). [] means no match.',
      inputSchema: z.object({
        query: z.string().describe('the address as a person writes it'),
        k: z.number().int().min(1).max(10).default(1).describe('number of candidates'),
      }),
      execute: async ({ query, k }) => ({ results: g.geocode(query, { k }) }),
    }),
    gridpin_reverse: tool({
      description: 'Offline reverse geocoding: the nearest addresses to a point, each with distance_m.',
      inputSchema: z.object({
        lat: z.number().min(-90).max(90),
        lon: z.number().min(-180).max(180),
        k: z.number().int().min(1).max(10).default(1),
      }),
      execute: async ({ lat, lon, k }) => ({ results: g.reverse(lat, lon, { k }) }),
    }),
  };
}
