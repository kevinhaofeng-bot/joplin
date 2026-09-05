const assert = require('node:assert/strict');

const { validateMachODependencies, validateRelativeBundlePath } = require('./build-sidecar-bundle.cjs');

assert.equal(validateRelativeBundlePath('sidecar.cjs'), true);
assert.equal(validateRelativeBundlePath('bin/node'), true);
assert.equal(validateRelativeBundlePath('../outside'), false);
assert.equal(validateRelativeBundlePath('/absolute/path'), false);
assert.equal(validateRelativeBundlePath(''), false);

assert.throws(() => validateMachODependencies('node:\n/opt/homebrew/opt/libuv/lib/libuv.1.dylib'), /non-system/);
assert.doesNotThrow(() => validateMachODependencies('node:\n/System/Library/Frameworks/Security.framework/Versions/A/Security\n/usr/lib/libSystem.B.dylib'));

console.log('sidecar bundle path contract: ok');
