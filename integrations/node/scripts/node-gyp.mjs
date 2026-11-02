// Run the node-gyp that npm bundles, wherever this install keeps npm.
//
//   node scripts/node-gyp.mjs rebuild
//
// npm sits under the global prefix (`npm root -g`) on most installs, but next to node.exe on the Windows
// runner, whose image points the global prefix to another folder; Homebrew is the other way round. Both
// places are tried; none found stops with the paths that were looked at.
import { execFileSync, spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import path from 'node:path';

const require = createRequire(import.meta.url);
const nodeDir = path.dirname(process.execPath);
const places = [path.join(nodeDir, 'node_modules', 'npm'), path.join(nodeDir, '..', 'lib', 'node_modules', 'npm')];
try {
  const root = execFileSync('npm', ['root', '-g'], { encoding: 'utf8', shell: process.platform === 'win32' });
  places.unshift(path.join(root.trim(), 'npm'));
} catch {
  // no npm on PATH: the places next to node remain
}
let gyp;
try {
  gyp = require.resolve('node-gyp/bin/node-gyp.js', { paths: places });
} catch {
  console.error(`node-gyp bundled with npm not found under: ${places.join(', ')}`);
  process.exit(1);
}
const run = spawnSync(process.execPath, [gyp, ...process.argv.slice(2)], { stdio: 'inherit' });
process.exit(run.status ?? 1);
