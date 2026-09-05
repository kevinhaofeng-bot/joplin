const assert = require('node:assert/strict');

const { validateRelativeBundlePath } = require('./build-sidecar-bundle.cjs');

assert.equal(validateRelativeBundlePath('sidecar.cjs'), true);
assert.equal(validateRelativeBundlePath('bin/node'), true);
assert.equal(validateRelativeBundlePath('../outside'), false);
assert.equal(validateRelativeBundlePath('/absolute/path'), false);
assert.equal(validateRelativeBundlePath(''), false);

console.log('sidecar bundle path contract: ok');
