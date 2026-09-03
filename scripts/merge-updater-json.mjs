#!/usr/bin/env node

import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, '..');

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i += 2) {
    out[argv[i].replace(/^--/, '')] = argv[i + 1];
  }
  return out;
}

const args = parseArgs(process.argv.slice(2));
const assetsDir = args.assets;
const repo = args.repo; 
const tag = args.tag; 
const notes = args.notes ?? '';
const outFile = args.out ?? 'latest.json';

if (!assetsDir || !repo || !tag) {
  console.error(
    '用法: node scripts/merge-updater-json.mjs --assets <目录> --repo <owner/repo> --tag <vX.Y.Z> [--notes ".."] [--out latest.json]'
  );
  process.exit(1);
}

const { version } = JSON.parse(
  readFileSync(join(repoRoot, 'src-tauri', 'tauri.conf.json'), 'utf8')
);


function platformKey(filename) {
  if (filename.endsWith('.exe')) return 'windows-x86_64'; 
  if (filename.endsWith('.app.tar.gz')) {
    if (filename.includes('aarch64')) return 'darwin-aarch64';
    if (filename.includes('universal')) return 'darwin-universal';
    if (filename.includes('x64') || filename.includes('amd64')) return 'darwin-x86_64';
  }
  if (filename.endsWith('.AppImage')) {
    if (filename.includes('aarch64') || filename.includes('arm64')) return 'linux-aarch64';
    if (filename.includes('amd64') || filename.includes('x86_64')) return 'linux-x86_64';
  }
  return null;
}

const platforms = {};
for (const file of readdirSync(assetsDir)) {
  if (!file.endsWith('.sig')) continue;
  const base = file.slice(0, -'.sig'.length); 
  const key = platformKey(base);
  if (!key) continue;
  const signature = readFileSync(join(assetsDir, file), 'utf8').trim();
  const url = `https://github.com/${repo}/releases/download/${tag}/${encodeURIComponent(base)}`;
  platforms[key] = { signature, url };
  console.log(`  [OK] ${key.padEnd(18)} <- ${base}`);
}

if (Object.keys(platforms).length === 0) {
  console.error(
    '未找到任何 .sig 签名文件。请确认 TAURI_SIGNING_PRIVATE_KEY 已配置且构建任务确实产出了签名。'
  );
  process.exit(1);
}

const manifest = {
  version,
  notes,
  pub_date: new Date().toISOString(),
  platforms,
};

const outPath = join(assetsDir, outFile);
writeFileSync(outPath, JSON.stringify(manifest, null, 2), 'utf8');
console.log(`==> 已生成 ${outPath}（${Object.keys(platforms).length} 个平台）`);
