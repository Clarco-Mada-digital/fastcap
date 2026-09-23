#!/usr/bin/env node
// Rédige la note de version publiée sur GitHub.
//
// Deux sources, dans l'ordre de priorité :
//   1. la section correspondante de CHANGELOG.md, si elle existe ;
//   2. à défaut, les commits depuis le tag précédent, regroupés par type.
//
// Le résultat est écrit au format `$GITHUB_OUTPUT` (body<<EOF … EOF) quand le
// script tourne dans une action, et en clair sinon.

import { execSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';

const tag = process.argv[2] ?? '';
const version = tag.replace(/^v/, '');

/** Commandes git tolérantes : un dépôt sans historique ne doit pas tout casser */
function git(command) {
  try {
    return execSync(`git ${command}`, {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
  } catch {
    return '';
  }
}

/** Section d'un CHANGELOG « Keep a Changelog » pour la version demandée */
function fromChangelog() {
  if (!existsSync('CHANGELOG.md')) return null;

  const content = readFileSync('CHANGELOG.md', 'utf8');
  const lines = content.split('\n');

  const start = lines.findIndex(
    (line) => /^##\s/.test(line) && line.includes(version)
  );
  if (start === -1) return null;

  const rest = lines.slice(start + 1);
  const end = rest.findIndex((line) => /^##\s/.test(line));
  const body = (end === -1 ? rest : rest.slice(0, end)).join('\n').trim();

  return body.length > 0 ? body : null;
}

/** Préfixes de commit usuels, traduits en rubriques */
const SECTIONS = [
  { key: 'feat', title: 'Nouveautés' },
  { key: 'fix', title: 'Corrections' },
  { key: 'perf', title: 'Performances' },
  { key: 'docs', title: 'Documentation' },
];

/** Note reconstruite depuis les commits, à défaut de CHANGELOG */
function fromCommits() {
  const previous = git('describe --tags --abbrev=0 HEAD^');
  const range = previous ? `${previous}..HEAD` : 'HEAD';
  const log = git(`log ${range} --pretty=format:%s --no-merges`);

  if (!log) return '_Première version._';

  const commits = log.split('\n').filter(Boolean);
  const grouped = new Map();
  const others = [];

  for (const subject of commits) {
    const match = subject.match(/^(\w+)(?:\([^)]*\))?!?:\s*(.+)$/);
    const section = match && SECTIONS.find((s) => s.key === match[1]);

    if (section) {
      if (!grouped.has(section.title)) grouped.set(section.title, []);
      grouped.get(section.title).push(match[2]);
    } else {
      others.push(subject);
    }
  }

  const parts = [];
  for (const { title } of SECTIONS) {
    const entries = grouped.get(title);
    if (entries?.length) {
      parts.push(`### ${title}\n${entries.map((e) => `- ${e}`).join('\n')}`);
    }
  }
  if (others.length) {
    parts.push(`### Divers\n${others.map((e) => `- ${e}`).join('\n')}`);
  }

  if (previous) {
    parts.push(`\n**Changements complets :** \`${previous}\` → \`${tag}\``);
  }

  return parts.join('\n\n');
}

const body = `${fromChangelog() ?? fromCommits()}

---

### Installation

| Système | Fichier |
| --- | --- |
| Windows | \`.msi\` ou \`.exe\` |
| macOS | \`.dmg\` (Apple Silicon ou Intel selon votre machine) |
| Linux | \`.AppImage\` ou \`.deb\` |

**ffmpeg est requis** pour enregistrer l'écran. Si FastCap ne le trouve pas, il
propose de l'installer depuis le panneau « Dépendances ».

Pour relire les vidéos dans l'application sous Linux, installez aussi un
décodeur H.264 : \`sudo apt install gstreamer1.0-libav\`.
`;

// Dans une action GitHub, la sortie multiligne passe par un délimiteur
if (process.env.GITHUB_OUTPUT || process.env.CI) {
  const delimiter = `EOF_${Math.random().toString(36).slice(2)}`;
  process.stdout.write(`body<<${delimiter}\n${body}\n${delimiter}\n`);
} else {
  process.stdout.write(`${body}\n`);
}
