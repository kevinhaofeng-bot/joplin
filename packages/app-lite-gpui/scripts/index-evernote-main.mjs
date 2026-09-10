#!/usr/bin/env node

import fs from "node:fs";

const inputPath = process.argv[2];

if (!inputPath) {
  process.stderr.write(
    "Usage: node index-evernote-main.mjs <beautified-main.js>\n",
  );
  process.exit(2);
}

const source = fs.readFileSync(inputPath, "utf8");
const lines = source.split(/\r?\n/);
const moduleStarts = [];

for (let index = 0; index < lines.length; index += 1) {
  const match = lines[index].match(/^\s{8}(\d+): function\b/);
  if (match) {
    moduleStarts.push({
      id: Number(match[1]),
      startLine: index + 1,
    });
  }
}

const markers = [
  "onAppReadyPreConduit",
  "initDataLayer",
  "initConduit",
  "onAppReadyPostConduit",
  "boronConduitWorker",
  "boronMain.html",
  "boronTabShell.html",
  "MainWindowTabManager",
  "BrowserWindow",
  "before-quit",
  "RTE_UPDATE_DOCUMENT_FROM_CE",
  "Offline_Search_Note_Content",
];

const modules = moduleStarts.map((moduleStart, index) => {
  const nextStart = moduleStarts[index + 1]?.startLine ?? lines.length + 1;
  const endLine = nextStart - 1;
  const body = lines.slice(moduleStart.startLine - 1, endLine).join("\n");
  const loggerLabels = new Set();
  const loggerPattern = /createLogger\)\((["'`])([^"'`]+)\1\)/g;

  for (const match of body.matchAll(loggerPattern)) {
    loggerLabels.add(match[2]);
  }

  return {
    id: moduleStart.id,
    startLine: moduleStart.startLine,
    endLine,
    lineCount: endLine - moduleStart.startLine + 1,
    loggerLabels: [...loggerLabels].sort(),
    markers: markers.filter((marker) => body.includes(marker)),
  };
});

process.stdout.write(
  `${JSON.stringify(
    {
      input: inputPath,
      lineCount: lines.length,
      moduleCount: modules.length,
      modules,
    },
    null,
    2,
  )}\n`,
);
