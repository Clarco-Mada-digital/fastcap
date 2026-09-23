#!/usr/bin/env node
// `npm run release <version>` — publie une nouvelle version.
//
// Le script aligne les numéros de version des trois fichiers qui les portent
// (package.json, Cargo.toml, tauri.conf.json), commite, pose le tag et le
// pousse. C'est le tag qui déclenche le workflow GitHub Actions, lequel
// compile les installeurs Windows, macOS et Linux.
//
//   npm run release 1.2.0
//   npm run release 1.2.0 -- --dry-run

import { execSync } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';

const args = process.argv.slice(2);
const dryRun = args.includes('--dry-run');
const version = args.find((arg) => !arg.startsWith('-'));

const RED = '\x1b[31m';
const GREEN = '\x1b[32m';
const DIM = '\x1b[2m';
const RESET = '\x1b[0m';

function fail(message) {
  console.error(`${RED}✗${RESET} ${message}`);
  process.exit(1);
}

function step(message) {
  console.log(`${GREEN}›${RESET} ${message}`);
}

function run(command) {
  if (dryRun) {
    console.log(`${DIM}  (simulation) ${command}${RESET}`);
    return '';
  }
  return execSync(command, { stdio: 'pipe', encoding: 'utf8' }).trim();
}

// --- Vérifications préalables ---

if (!version) {
  fail('Version manquante.\n  Exemple : npm run release 1.2.0');
}

if (!/^\d+\.\d+\.\d+(-[\w.]+)?$/.test(version)) {
  fail(`Version « ${version} » invalide : attendu MAJEUR.MINEUR.CORRECTIF (ex. 1.2.0)`);
}

const tag = `v${version}`;

try {
  execSync('git rev-parse --is-inside-work-tree', { stdio: 'ignore' });
} catch {
  fail(
    "Ce dossier n'est pas un dépôt git.\n" +
      '  Initialisez-le puis reliez-le à GitHub :\n' +
      '    git init && git add -A && git commit -m "init"\n' +
      '    git remote add origin git@github.com:<utilisateur>/<dépôt>.git'
  );
}

// Un arbre sale produirait un tag dont le contenu ne correspond à rien
const dirty = execSync('git status --porcelain', { encoding: 'utf8' }).trim();
if (dirty && !dryRun) {
  fail(
    `Des modifications ne sont pas commitées :\n${dirty}\n` +
      '  Commitez-les ou mettez-les de côté avant de publier.'
  );
}

const existing = execSync('git tag --list', { encoding: 'utf8' }).split('\n');
if (existing.includes(tag)) {
  fail(`Le tag ${tag} existe déjà.`);
}

let remote = '';
try {
  remote = execSync('git remote get-url origin', { encoding: 'utf8' }).trim();
} catch {
  fail(
    'Aucun dépôt distant « origin » : impossible de déclencher la publication.\n' +
      '    git remote add origin git@github.com:<utilisateur>/<dépôt>.git'
  );
}

// --- Mise à jour des numéros de version ---

/** Remplace la version dans un fichier, en ne touchant que la bonne ligne */
function bump(path, pattern, replacement) {
  const before = readFileSync(path, 'utf8');
  const after = before.replace(pattern, replacement);

  if (before === after) {
    fail(`Version introuvable dans ${path} — format inattendu.`);
  }

  if (!dryRun) writeFileSync(path, after);
  step(`${path} → ${version}`);
}

bump('package.json', /("version":\s*")[^"]+(")/, `$1${version}$2`);
bump(
  'src-tauri/tauri.conf.json',
  /("version":\s*")[^"]+(")/,
  `$1${version}$2`
);
// Uniquement la version du paquet, en tête de Cargo.toml — pas celles des dépendances
bump(
  'src-tauri/Cargo.toml',
  /(^\[package\][\s\S]*?\nversion\s*=\s*")[^"]+(")/m,
  `$1${version}$2`
);

// Cargo.lock doit suivre, sinon la compilation échoue en CI (--locked)
step('Mise à jour de Cargo.lock');
run('cargo update --manifest-path src-tauri/Cargo.toml --workspace');

// --- Journal des modifications ---

// `release-notes.mjs` retient la section portant le numéro publié. Tant que
// les nouveautés restent sous « [Non publié] », elle ne trouve rien et se
// rabat sur les messages de commit : la note de version rédigée à la main
// serait perdue. On promeut donc la section, et on en rouvre une vide.
function promoteChangelog() {
  const path = 'CHANGELOG.md';
  if (!existsSync(path)) {
    fail('CHANGELOG.md est introuvable : la note de version serait vide.');
  }

  const before = readFileSync(path, 'utf8');
  const heading = /^##\s*\[Non publié\][^\n]*$/m;
  const match = before.match(heading);

  if (!match) {
    fail(
      'Aucune section « ## [Non publié] » dans CHANGELOG.md.\n' +
        '  C\'est elle qui devient la note de version publiée.'
    );
  }

  // Une section vide donnerait une release sans la moindre explication
  const rest = before.slice(match.index + match[0].length);
  const next = rest.search(/^##\s/m);
  const body = (next === -1 ? rest : rest.slice(0, next)).trim();

  if (body.length === 0) {
    fail('La section « [Non publié] » est vide : rien à annoncer.');
  }

  const today = new Date().toISOString().slice(0, 10);
  const after = before.replace(
    heading,
    `## [Non publié]\n\n## [${version}] — ${today}`
  );

  if (!dryRun) writeFileSync(path, after);
  step(`CHANGELOG.md → section [${version}] (${today})`);
}

promoteChangelog();

// --- Contrôles avant publication ---

step('Tests');
run('cargo test --manifest-path src-tauri/Cargo.toml --lib');

step('Compilation du frontend');
run('npm run build');

// --- Commit, tag, poussée ---

step(`Commit et tag ${tag}`);
run(
  'git add package.json src-tauri/tauri.conf.json src-tauri/Cargo.toml ' +
    'src-tauri/Cargo.lock CHANGELOG.md'
);
run(`git commit -m "release: ${version}"`);
run(`git tag -a ${tag} -m "FastCap ${version}"`);

const branch = execSync('git rev-parse --abbrev-ref HEAD', { encoding: 'utf8' }).trim();
step(`Envoi vers origin (${branch} + ${tag})`);
run(`git push origin ${branch}`);
run(`git push origin ${tag}`);

// --- Conclusion ---

const slug = remote
  .replace(/^git@github\.com:/, '')
  .replace(/^https:\/\/github\.com\//, '')
  .replace(/\.git$/, '');

console.log('');
if (dryRun) {
  console.log(`${DIM}Simulation terminée : rien n'a été écrit ni poussé.${RESET}`);
} else {
  console.log(`${GREEN}✓${RESET} FastCap ${version} est en cours de publication.`);
  console.log(`  Suivi    : https://github.com/${slug}/actions`);
  console.log(`  Résultat : https://github.com/${slug}/releases/tag/${tag}`);
  console.log('');
  console.log(
    `${DIM}  Les installeurs Windows, macOS (Intel et Apple Silicon) et Linux`
  );
  console.log(`  sont compilés en parallèle — comptez une vingtaine de minutes.${RESET}`);
}
