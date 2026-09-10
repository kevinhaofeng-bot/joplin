#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fixture_dir="$script_dir/test-fixtures/evernote-reconstruct"
test_tmp="$(mktemp -d)"
trap 'rm -rf "$test_tmp"' EXIT

node "$script_dir/index-evernote-main.mjs" "$fixture_dir/beautified-main.js" \
  | node -e '
let input = "";
process.stdin.on("data", (chunk) => (input += chunk));
process.stdin.on("end", () => {
  const index = JSON.parse(input);
  if (index.moduleCount !== 3 || !index.modules.some((item) => item.id === 7000)) {
    throw new Error("module index did not canonicalize scientific notation ID 7e3");
  }
});
'

node "$script_dir/reconstruct-evernote-main.mjs" \
  --beautified "$fixture_dir/beautified-main.js" \
  --webcrack "$fixture_dir/webcrack" \
  --output "$test_tmp/output" \
  --version "test-build"

node - "$test_tmp/output" <<'NODE'
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const output = process.argv[2];
const manifest = JSON.parse(
  fs.readFileSync(path.join(output, "manifest.json"), "utf8"),
);

if (manifest.moduleCount !== 3) {
  throw new Error(`expected 3 real modules, got ${manifest.moduleCount}`);
}
if (manifest.missingModules.length !== 0) {
  throw new Error(`missing modules: ${manifest.missingModules.join(", ")}`);
}
if (JSON.stringify(manifest.ignoredWebcrackCandidates) !== '[]') {
  throw new Error(
    `expected no ignored candidates, got ${JSON.stringify(manifest.ignoredWebcrackCandidates)}`,
  );
}

const moduleOne = manifest.modules.find((item) => item.id === "1");
const moduleTwo = manifest.modules.find((item) => item.id === "2");
const moduleSevenThousand = manifest.modules.find((item) => item.id === "7000");
if (!moduleSevenThousand) {
  throw new Error("scientific-notation module ID 7e3 was not recovered as 7000");
}
if (!moduleOne.outputPath.endsWith("00001__demo-controller.js")) {
  throw new Error(`unexpected semantic path: ${moduleOne.outputPath}`);
}

const moduleOneCode = fs.readFileSync(
  path.join(output, moduleOne.outputPath),
  "utf8",
);
const moduleTwoCode = fs.readFileSync(
  path.join(output, moduleTwo.outputPath),
  "utf8",
);
const moduleSevenThousandCode = fs.readFileSync(
  path.join(output, moduleSevenThousand.outputPath),
  "utf8",
);
const entryCode = fs.readFileSync(path.join(output, "src/main.js"), "utf8");

if (!moduleOneCode.includes(`require("./${path.basename(moduleTwo.outputPath)}")`)) {
  throw new Error("numeric dependency was not rewritten to the semantic module path");
}
if (!moduleTwoCode.includes('require("path")')) {
  throw new Error("external Node dependency was not restored");
}
if (!entryCode.includes(`require("./modules/${path.basename(moduleOne.outputPath)}")`)) {
  throw new Error("entry point does not reference the reconstructed module tree");
}
if (!fs.existsSync(path.join(output, "dependency-graph.json"))) {
  throw new Error("dependency graph was not generated");
}
if (!fs.existsSync(path.join(output, moduleSevenThousand.outputPath))) {
  throw new Error("scientific-notation module was not written to the source tree");
}
new vm.Script(moduleSevenThousandCode, {
  filename: moduleSevenThousand.outputPath,
});
if (!moduleSevenThousandCode.includes("value === undefined")) {
  throw new Error("illegal strict/default parameter combination was not repaired");
}

console.log("reconstruct-evernote-main fixture: PASS");
NODE
