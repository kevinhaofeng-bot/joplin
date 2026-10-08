"""Exercise real bundle packaging, replacing only Cargo compilation/cleanup.

The missing/wrong parent startup policy must fail these tests. The generated
bundle's plist, picker isolation, icon and code signatures are real; the stub
binary deliberately does not claim to verify Rust or GUI behavior.
"""
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import unittest

PROJECT = Path(__file__).resolve().parent.parent


class MacosPackagePolicy(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='package-policy-', dir=PROJECT/'tests')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        scripts = self.root/'scripts'
        scripts.mkdir()
        for name in ('package-notes-macos.sh','package-picker-helper.sh','cargo-notes.sh','make-app-icon.sh'):
            shutil.copy2(PROJECT/'scripts'/name, scripts/name)
        (self.root/'assets').mkdir()
        shutil.copy2(PROJECT/'assets/AppIcon-dock-v2.png',self.root/'assets/AppIcon-dock-v2.png')
        shutil.copy2(PROJECT/'Cargo.toml',self.root/'Cargo.toml')
        shutil.copy2(PROJECT/'Cargo.lock',self.root/'Cargo.lock')
        self.target = self.root/'fixture-target'
        (self.target/'release').mkdir(parents=True)
        # Do not propagate Apple's protected filesystem flags to test files.
        shutil.copy('/usr/bin/true',self.target/'release/velotype')
        shim = self.root/'bin'
        shim.mkdir()
        cargo = shim/'cargo'
        cargo.write_text('''#!/usr/bin/env python3
import json,os,sys
args=sys.argv[1:]
if args and args[0]=='metadata':
    assert '--no-deps' in args and '--manifest-path' in args
    print(json.dumps({'target_directory':os.environ['PACKAGE_TEST_TARGET']},separators=(',',':')))
elif args and args[0]=='clean':
    assert '--offline' in args and '--locked' in args
    assert args[args.index('--target-dir')+1]==os.environ['PACKAGE_TEST_TARGET']
elif args and args[0]=='build':
    assert '--locked' in args and '--release' in args
    assert args[args.index('--bin')+1]=='velotype'
else:
    raise SystemExit('unexpected cargo fixture operation')
''')
        cargo.chmod(0o700)
        self.env = dict(os.environ)
        for key in ('JOPLIN_LITE_ACCEPTANCE_ID','JOPLIN_LITE_ACCEPTANCE_PROFILE',
                    'JOPLIN_LITE_MACOS_MEMORY_POLICY','JOPLIN_LITE_SIGN_IDENTITY'):
            self.env.pop(key,None)
        self.env['PATH'] = str(shim)+os.pathsep+self.env['PATH']
        self.env['PACKAGE_TEST_TARGET'] = str(self.target)
        self.env['JOPLIN_LITE_SIGN_IDENTITY'] = '-'
        self.output = self.root/'中文 有空格'

    def package(self, policy=None, acceptance=False):
        env = dict(self.env)
        if policy is not None:
            env['JOPLIN_LITE_MACOS_MEMORY_POLICY'] = policy
        if acceptance:
            env['JOPLIN_LITE_ACCEPTANCE_ID'] = 'com.arielkevin.joplinlite.acceptance.policytest'
            env['JOPLIN_LITE_ACCEPTANCE_PROFILE'] = str(self.root/'原资料不打开')
        result = subprocess.run(['bash',str(self.root/'scripts/package-notes-macos.sh'),str(self.output)],
                                env=env,text=True,capture_output=True,timeout=90)
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        apps = list(self.output.glob('*/Joplin Lite.app'))
        self.assertEqual(len(apps),1)
        app = apps[0]
        metadata = plistlib.loads((app/'Contents/Info.plist').read_bytes())
        helper = app/'Contents/Helpers/Joplin Lite Picker.app'
        picker = plistlib.loads((helper/'Contents/Info.plist').read_bytes())
        self.assertEqual(picker['CFBundleIdentifier'],metadata['CFBundleIdentifier']+'.picker')
        self.assertTrue(picker['LSUIElement'])
        self.assertNotIn('JoplinLiteAcceptanceProfile',picker)
        self.assertNotIn('LSEnvironment',picker)
        self.assertNotIn('CFBundleDocumentTypes',metadata)
        self.assertNotIn('CFBundleDocumentTypes',picker)
        self.assertGreater((app/'Contents/Resources/AppIcon.icns').stat().st_size,1024)
        subprocess.run(['/usr/bin/codesign','--verify','--deep','--strict',str(app)],check=True,capture_output=True)
        return app,metadata,(app.parent/'BUILD-INFO.txt').read_text()

    def test_default_package_applies_only_parent_allocator_policy(self):
        app,metadata,report = self.package()
        self.assertEqual(metadata.get('LSEnvironment'),{'MallocMediumZone':'0'})
        self.assertIn('macos_allocator_policy: medium-disabled',report)
        self.assertNotIn('JoplinLiteAcceptanceProfile',metadata)

    def test_acceptance_pin_survives_scoped_policy_and_signing(self):
        app,metadata,report = self.package(acceptance=True)
        self.assertEqual(metadata['JoplinLiteAcceptanceProfile'],str(self.root/'原资料不打开'))
        self.assertEqual(metadata['CFBundleIdentifier'],'com.arielkevin.joplinlite.acceptance.policytest')
        self.assertEqual(metadata.get('LSEnvironment'),{'MallocMediumZone':'0'})
        self.assertFalse((self.root/'原资料不打开').exists())

    def test_system_default_rollback_has_no_allocator_override(self):
        app,metadata,report = self.package(policy='system-default')
        self.assertNotIn('LSEnvironment',metadata)
        self.assertIn('macos_allocator_policy: system-default',report)

    def test_unknown_policy_refused_before_build_or_bundle_creation(self):
        env = dict(self.env,JOPLIN_LITE_MACOS_MEMORY_POLICY='unexpected-value')
        result = subprocess.run(['bash',str(self.root/'scripts/package-notes-macos.sh'),str(self.output)],
                                env=env,text=True,capture_output=True,timeout=90)
        self.assertNotEqual(result.returncode,0)
        self.assertIn('JOPLIN_LITE_MACOS_MEMORY_POLICY',result.stderr)
        self.assertFalse(self.output.exists())


if __name__ == '__main__':
    unittest.main(verbosity=2)
