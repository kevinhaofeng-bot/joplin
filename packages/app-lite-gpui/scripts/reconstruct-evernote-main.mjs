#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const knownModuleNames = new Map([
  [4529, "broker-bridge"],
  [14827, "user-data-move-window-controller"],
  [20653, "force-update-window-controller"],
  [32683, "popup-note-window-controller"],
  [52188, "external-en-conduit-electron"],
  [54141, "window-controller"],
  [54987, "menu-builder"],
  [57075, "external-conduit-view-types"],
  [58919, "main-window-controller"],
  [61978, "main-window-tab-manager"],
  [62086, "application-constants"],
  [62264, "application-controller"],
  [62656, "external-conduit-utils"],
  [71174, "local-settings"],
  [71017, "external-node-path"],
  [72298, "external-electron"],
  [72879, "system-audio-capture-initializer"],
  [83456, "broker-client"],
  [93824, "application-paths"],
  [95385, "environment"],
  [95913, "login-window-controller"],
]);

function parseArguments(argv) {
  const values = new Map();
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index];
    const value = argv[index + 1];
    if (!key?.startsWith("--") || value === undefined) {
      throw new Error(`Invalid argument near ${key ?? "<end>"}`);
    }
    values.set(key.slice(2), value);
  }

  const required = ["beautified", "webcrack", "output", "version"];
  for (const key of required) {
    if (!values.has(key)) {
      throw new Error(`Missing required argument --${key}`);
    }
  }
  return Object.fromEntries(values);
}

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function toKebabCase(value) {
  return value
    .replace(/^.*:/, "")
    .replace(/([a-z0-9])([A-Z])/g, "$1-$2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1-$2")
    .replace(/[^A-Za-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .toLowerCase()
    .slice(0, 80);
}

function externalNameFromSource(source) {
  const match = source.match(
    /require\(\/\*webcrack:missing\*\/(["'])\.\/([^"']+)\1\)/,
  );
  if (!match) return null;
  return match[2].replace(/\.js$/, "");
}

function loggerNamesFromSource(source) {
  const names = new Set();
  const pattern = /createLogger\)\((["'`])([^"'`]+)\1\)/g;
  for (const match of source.matchAll(pattern)) {
    names.add(match[2]);
  }
  return [...names];
}

function exportedNamesFromSource(source) {
  const names = new Set();
  const getterPattern = /\bget\s+([A-Za-z_$][\w$]*)\s*\(\)/g;
  const propertyPattern = /Object\.defineProperty\(exports,\s*["']([^"']+)["']/g;
  for (const match of source.matchAll(getterPattern)) names.add(match[1]);
  for (const match of source.matchAll(propertyPattern)) names.add(match[1]);
  names.delete("default");
  names.delete("__esModule");
  return [...names].sort();
}

function inferredModuleName(id, source) {
  const known = knownModuleNames.get(Number(id));
  if (known) return known;

  const externalName = externalNameFromSource(source);
  if (externalName) return `external-${toKebabCase(externalName)}`;

  const loggerNames = loggerNamesFromSource(source);
  if (loggerNames.length > 0) return toKebabCase(loggerNames[0]);

  if (
    source.length > 100_000 &&
    /module\.exports\s*=\s*\{\s*A2S\s*:/.test(source)
  ) {
    return "localization-catalog";
  }

  const exports = exportedNamesFromSource(source).filter(
    (name) => name.length > 2 && !name.startsWith("__"),
  );
  if (exports.length > 0 && exports.length <= 4) {
    return toKebabCase(exports.join("-"));
  }

  return `module-${id}`;
}

function outputFilename(id, source) {
  return `${String(id).padStart(5, "0")}__${inferredModuleName(id, source)}.js`;
}

function numericDependencies(source) {
  const ids = new Set();
  for (const match of source.matchAll(/require\((["'])\.\/(\d+)\.js\1\)/g)) {
    ids.add(match[2]);
  }
  return [...ids].sort((left, right) => Number(left) - Number(right));
}

function repairIllegalStrictDefaultParameters(source) {
  return source.replace(
    /function(\s+[A-Za-z_$][\w$]*)?\s*\(\s*([A-Za-z_$][\w$]*)\s*=\s*([^\n)]+)\)\s*\{\n([ \t]*)["']use strict["'];/g,
    (_whole, functionName = "", parameter, defaultValue, indentation) =>
      `function${functionName} (${parameter}) {\n${indentation}"use strict";\n\n${indentation}if (${parameter} === undefined) {\n${indentation}  ${parameter} = ${defaultValue.trim()};\n${indentation}}`,
  );
}

function rewriteModuleSource(source, filenameById) {
  let rewritten = repairIllegalStrictDefaultParameters(source).replace(
    /require\(\/\*webcrack:missing\*\/(["'])\.\/([^"']+)\1\)/g,
    (_whole, _quote, filename) =>
      `require(${JSON.stringify(filename.replace(/\.js$/, ""))})`,
  );

  rewritten = rewritten.replace(
    /require\((["'])\.\/(\d+)\.js\1\)/g,
    (_whole, _quote, id) => {
      const filename = filenameById.get(id);
      if (!filename) return _whole;
      return `require(${JSON.stringify(`./${filename}`)})`;
    },
  );
  return rewritten.endsWith("\n") ? rewritten : `${rewritten}\n`;
}

function extractEntrySource(deobfuscatedSource, filenameById) {
  const marker = "var __webpack_exports__ = {};";
  const start = deobfuscatedSource.lastIndexOf(marker);
  if (start < 0) {
    throw new Error("Could not locate the Webpack entry marker");
  }

  const exportMarker = "module.exports = __webpack_exports__;";
  const exportStart = deobfuscatedSource.indexOf(exportMarker, start);
  if (exportStart < 0) {
    throw new Error("Could not locate the Webpack entry export");
  }
  const end = exportStart + exportMarker.length;
  return deobfuscatedSource.slice(start, end).replace(
    /__webpack_require__\((\d+)\)/g,
    (_whole, id) => {
      const filename = filenameById.get(id);
      if (!filename) {
        throw new Error(`Entry references missing module ${id}`);
      }
      return `require(${JSON.stringify(`./modules/${filename}`)})`;
    },
  );
}

function writeJson(target, value) {
  fs.writeFileSync(target, `${JSON.stringify(value, null, 2)}\n`);
}

function listFilesRecursively(root) {
  const files = [];
  for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
    const target = path.join(root, entry.name);
    if (entry.isDirectory()) files.push(...listFilesRecursively(target));
    else files.push(target);
  }
  return files;
}

function csvCell(value) {
  return `"${String(value).replaceAll('"', '""')}"`;
}

const args = parseArguments(process.argv.slice(2));
const beautifiedPath = path.resolve(args.beautified);
const webcrackPath = path.resolve(args.webcrack);
const outputPath = path.resolve(args.output);

if (fs.existsSync(outputPath)) {
  throw new Error(`Output already exists: ${outputPath}`);
}

const beautifiedSource = fs.readFileSync(beautifiedPath, "utf8");
const deobfuscatedPath = path.join(webcrackPath, "deobfuscated.js");
const bundleMetadataPath = path.join(webcrackPath, "bundle.json");
const deobfuscatedSource = fs.readFileSync(deobfuscatedPath, "utf8");
const webcrackMetadata = JSON.parse(
  fs.readFileSync(bundleMetadataPath, "utf8"),
);

const topLevelModuleIds = [
  ...beautifiedSource.matchAll(
    /^ {8}(0[xX][0-9a-fA-F]+|\d+(?:[eE][+-]?\d+)?): function\b/gm,
  ),
].map((match) => String(Number(match[1])));
const uniqueTopLevelIds = [...new Set(topLevelModuleIds)].sort(
  (left, right) => Number(left) - Number(right),
);

if (uniqueTopLevelIds.length !== topLevelModuleIds.length) {
  throw new Error("Duplicate top-level Webpack module IDs detected");
}

const webcrackIds = webcrackMetadata.modules.map((item) => String(item.id));
const topLevelSet = new Set(uniqueTopLevelIds);
const ignoredWebcrackCandidates = webcrackIds
  .filter((id) => !topLevelSet.has(id))
  .sort((left, right) => Number(left) - Number(right));
const missingModules = uniqueTopLevelIds.filter(
  (id) => !fs.existsSync(path.join(webcrackPath, `${id}.js`)),
);

if (missingModules.length > 0) {
  throw new Error(`Webcrack output is missing modules: ${missingModules.join(", ")}`);
}

const sourceById = new Map();
const filenameById = new Map();
for (const id of uniqueTopLevelIds) {
  const source = fs.readFileSync(path.join(webcrackPath, `${id}.js`), "utf8");
  sourceById.set(id, source);
  filenameById.set(id, outputFilename(id, source));
}

const modulesPath = path.join(outputPath, "src/modules");
const diagnosticPath = path.join(outputPath, "diagnostics");
const extraPath = path.join(diagnosticPath, "webcrack-extra");
fs.mkdirSync(modulesPath, { recursive: true });
fs.mkdirSync(extraPath, { recursive: true });

const modules = [];
const dependencyEdges = [];
for (const id of uniqueTopLevelIds) {
  const source = sourceById.get(id);
  const filename = filenameById.get(id);
  const dependencies = numericDependencies(source);
  const externalDependencies = [
    ...new Set(
      [...source.matchAll(/webcrack:missing\*\/(["'])\.\/([^"']+)\1/g)].map(
        (match) => match[2].replace(/\.js$/, ""),
      ),
    ),
  ].sort();
  const rewritten = rewriteModuleSource(source, filenameById);
  const outputRelativePath = `src/modules/${filename}`;
  fs.writeFileSync(path.join(outputPath, outputRelativePath), rewritten);

  for (const dependency of dependencies) {
    dependencyEdges.push({ from: id, to: dependency });
  }

  modules.push({
    id,
    name: inferredModuleName(id, source),
    outputPath: outputRelativePath,
    originalWebcrackPath: `${id}.js`,
    originalSha256: sha256(source),
    outputSha256: sha256(rewritten),
    bytes: Buffer.byteLength(rewritten),
    lines: rewritten.split(/\r?\n/).length,
    dependencies,
    externalDependencies,
    loggerNames: loggerNamesFromSource(source),
    exportedNames: exportedNamesFromSource(source),
  });
}

for (const id of ignoredWebcrackCandidates) {
  const source = path.join(webcrackPath, `${id}.js`);
  if (fs.existsSync(source)) {
    fs.copyFileSync(source, path.join(extraPath, `${id}.js`));
  }
}

const entrySource = extractEntrySource(deobfuscatedSource, filenameById);
fs.writeFileSync(
  path.join(outputPath, "src/main.js"),
  entrySource.endsWith("\n") ? entrySource : `${entrySource}\n`,
);
fs.copyFileSync(
  bundleMetadataPath,
  path.join(diagnosticPath, "webcrack-bundle.json"),
);
fs.copyFileSync(
  deobfuscatedPath,
  path.join(diagnosticPath, "deobfuscated-main.js"),
);

const manifest = {
  version: args.version,
  generatedAt: new Date().toISOString(),
  inputs: {
    beautifiedPath,
    beautifiedSha256: sha256(beautifiedSource),
    webcrackDeobfuscatedPath: deobfuscatedPath,
    webcrackDeobfuscatedSha256: sha256(deobfuscatedSource),
  },
  moduleCount: modules.length,
  missingModules,
  ignoredWebcrackCandidates,
  modules,
};
writeJson(path.join(outputPath, "manifest.json"), manifest);
writeJson(path.join(outputPath, "dependency-graph.json"), {
  nodes: modules.map(({ id, name, outputPath: moduleOutputPath }) => ({
    id,
    name,
    outputPath: moduleOutputPath,
  })),
  edges: dependencyEdges,
});

const csvRows = [
  ["id", "name", "output_path", "bytes", "dependencies", "externals"],
  ...modules.map((item) => [
    item.id,
    item.name,
    item.outputPath,
    item.bytes,
    item.dependencies.join(" "),
    item.externalDependencies.join(" "),
  ]),
];
fs.writeFileSync(
  path.join(outputPath, "module-map.csv"),
  `${csvRows.map((row) => row.map(csvCell).join(",")).join("\n")}\n`,
);

const namedModules = modules
  .filter((item) => !item.name.startsWith("module-"))
  .sort((left, right) => Number(left.id) - Number(right.id));
const readme = `# Evernote ${args.version} reconstructed main-process source

This directory is a best-effort readable reconstruction of the installed
desktop client's Webpack main-process bundle.

## Coverage

- Top-level modules proven by the beautified bundle: ${modules.length}
- Recovered module files: ${modules.length}
- Missing module files: ${missingModules.length}
- Webcrack candidates rejected by the top-level boundary check: ${ignoredWebcrackCandidates.length}
- Modules with a semantic or external label: ${namedModules.length}

The readable CommonJS entry is [src/main.js](src/main.js). All top-level
modules are in [src/modules](src/modules), with numeric IDs retained as a
stable prefix and inferred names added after the separator. Internal
\`require()\` paths have been rewritten to those semantic filenames.

The [manifest.json](manifest.json) records input and per-module hashes,
dependencies, exports, logger evidence and coverage. The complete graph is in
[dependency-graph.json](dependency-graph.json); [module-map.csv](module-map.csv)
is intended for sorting and manual annotation.

The original Webcrack full-bundle result and metadata are retained below
\`diagnostics/\` so every split module can be audited against the whole.
Candidates rejected as nested numeric keys are preserved under
\`diagnostics/webcrack-extra/\` rather than silently deleted.

## Recovery boundary

Formatting, module splitting, CommonJS dependencies, modern syntax, class
methods, string literals, export names and many product/logger names are
recovered. Local identifiers destroyed by minification cannot be recovered
exactly without the missing top-level source map; inferred filenames are
evidence-backed navigation aids, not claims about original paths.
`;
fs.writeFileSync(path.join(outputPath, "README.md"), readme);

const checksumFiles = listFilesRecursively(outputPath)
  .filter((file) => path.basename(file) !== "CHECKSUMS.sha256")
  .sort();
const checksums = checksumFiles.map((file) => {
  const relative = path.relative(outputPath, file);
  return `${sha256(fs.readFileSync(file))}  ${relative}`;
});
fs.writeFileSync(
  path.join(outputPath, "CHECKSUMS.sha256"),
  `${checksums.join("\n")}\n`,
);

process.stdout.write(
  `${JSON.stringify({
    output: outputPath,
    moduleCount: modules.length,
    ignoredWebcrackCandidates,
    semanticNames: namedModules.length,
    dependencyEdges: dependencyEdges.length,
  })}\n`,
);
