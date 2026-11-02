// Assemble the npm packages of a release from prebuilt native modules.
//
//   node scripts/platform-packages.mjs <native-dir> <out-dir> <version>
//
// <native-dir>/<platform>-<arch>/gridpin.node comes from the release workflow, one per PLATFORMS entry.
// Writes <out-dir>/<platform>-<arch>/ (package @gridpin/gridpin-<platform>-<arch>: the module, package.json
// with os/cpu so npm installs only the matching one, LICENSE and THIRDPARTY.yml - the license texts of the
// Rust libraries linked into the module) and <out-dir>/gridpin/ (the main package, its optionalDependencies
// pinned to the same version). A missing module, or an extra one, stops the script: a release ships all
// platforms or none.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const CODE = path.resolve(HERE, '..', '..');

// npm's process.platform / process.arch, plus libc for Linux (the modules are built on glibc runners).
export const PLATFORMS = [
  { platform: 'linux', arch: 'x64', libc: 'glibc' },
  { platform: 'linux', arch: 'arm64', libc: 'glibc' },
  { platform: 'darwin', arch: 'arm64' },
  { platform: 'win32', arch: 'x64' },
];

export const packageName = (p) => `@gridpin/gridpin-${p.platform}-${p.arch}`;

export function assemble(nativeDir, outDir, version) {
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`version must be X.Y.Z, got ${version}`);
  const found = fs.readdirSync(nativeDir).filter((d) => fs.existsSync(path.join(nativeDir, d, 'gridpin.node')));
  const wanted = PLATFORMS.map((p) => `${p.platform}-${p.arch}`);
  const missing = wanted.filter((w) => !found.includes(w));
  const extra = found.filter((f) => !wanted.includes(f));
  if (missing.length || extra.length) {
    throw new Error(`native modules: missing ${JSON.stringify(missing)}, unexpected ${JSON.stringify(extra)}`);
  }
  const main = JSON.parse(fs.readFileSync(path.join(HERE, 'package.json'), 'utf8'));
  const optional = {};
  for (const p of PLATFORMS) {
    const key = `${p.platform}-${p.arch}`;
    const dir = path.join(outDir, key);
    fs.mkdirSync(dir, { recursive: true });
    fs.copyFileSync(path.join(nativeDir, key, 'gridpin.node'), path.join(dir, 'gridpin.node'));
    fs.copyFileSync(path.join(HERE, 'LICENSE'), path.join(dir, 'LICENSE'));
    fs.copyFileSync(path.join(CODE, 'gridpin-ffi', 'THIRDPARTY.yml'), path.join(dir, 'THIRDPARTY.yml'));
    const pkg = {
      name: packageName(p),
      version,
      description: `GridPin native module for ${key} (installed by the gridpin package)`,
      license: main.license,
      homepage: main.homepage,
      repository: main.repository,
      main: 'gridpin.node',
      os: [p.platform],
      cpu: [p.arch],
      ...(p.libc ? { libc: [p.libc] } : {}),
      files: ['gridpin.node', 'LICENSE', 'THIRDPARTY.yml'],
    };
    fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify(pkg, null, 2) + '\n');
    optional[pkg.name] = version;
  }
  const mainDir = path.join(outDir, 'gridpin');
  fs.mkdirSync(mainDir, { recursive: true });
  for (const f of main.files) fs.copyFileSync(path.join(HERE, f), path.join(mainDir, f));
  fs.writeFileSync(path.join(mainDir, 'package.json'),
    JSON.stringify({ ...main, version, optionalDependencies: optional }, null, 2) + '\n');
  return { platforms: Object.keys(optional), main: mainDir };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [nativeDir, outDir, version] = process.argv.slice(2);
  if (!version) {
    console.error('usage: node scripts/platform-packages.mjs <native-dir> <out-dir> <version>');
    process.exit(2);
  }
  const { platforms, main } = assemble(nativeDir, outDir, version);
  console.log(`packages: ${platforms.join(', ')}; main package in ${main}`);
}
