import json
import contextlib
import io
from pathlib import Path
import plistlib
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
import build
import install
import patcher


def fixture(app, version='5.0.99', filename='index-new-asset-hash.js', renderer=None):
    resources = app / 'Contents/Resources'
    resources.mkdir(parents=True)
    empty_header = b'{"files":{}}'
    padded = (len(empty_header) + 3) & ~3
    archive = build.Archive(struct.pack('<IIII', 4, padded + 8, padded + 4, len(empty_header)) + empty_header + bytes(padded - len(empty_header)))
    files = {
        'package.json': json.dumps({'name': 'teamwire', 'version': version, 'main': 'build/electron/main.js'}).encode(),
        'build/index.html': f'<script type="module" src="./assets/{filename}"></script>'.encode(),
        f'build/assets/{filename}': (renderer or (ROOT / 'test/fixture.js').read_text()).encode(),
        'build/electron/main.js': b'new electron.BrowserWindow({webPreferences:{contextIsolation:true,nodeIntegration:false,sandbox:false,preload:path.join(__dirname,"preloads/main.js")}})',
        'build/electron/preloads/main.js': b'const {contextBridge}=require("electron");',
    }
    packed = archive.replace(files)
    (resources / 'app.asar').write_bytes(packed)
    info = {'CFBundleIdentifier': 'eu.teamwire.app', 'CFBundleShortVersionString': version,
            'ElectronAsarIntegrity': {'Resources/app.asar': {'algorithm': 'SHA256', 'hash': build.sha(build.Archive(packed).raw_header)}}}
    (app / 'Contents/Info.plist').write_bytes(plistlib.dumps(info))
    (app / 'Contents/MacOS').mkdir()
    (app / 'Contents/MacOS/Teamwire').write_bytes(b'fixture, not executable')
    (app / 'Contents/_CodeSignature').mkdir()
    (app / 'Contents/_CodeSignature/CodeResources').write_bytes(b'fixture signature')


class PatcherTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.app = self.root / 'Applications/Teamwire.app'
        self.state = self.root / 'state'
        self.config = {'app': str(self.app), 'node': shutil.which('node'), 'identity': 'TEST', 'settle_seconds': 15}
        fixture(self.app)

    def tearDown(self):
        self.temp.cleanup()

    def test_new_asset_name_and_synthetic_version_are_discovered(self):
        result = build.build(self.app, self.root / 'build.json')
        details = build.inspect(self.app)
        self.assertEqual(details['kind'], 'patched')
        self.assertEqual(details['version'], '5.0.99')
        self.assertEqual(details['manifest']['renderer_path'], 'build/assets/index-new-asset-hash.js')
        self.assertEqual(details['asar_sha256'], result['patched_asar_sha256'])

    def test_idempotence_does_not_append_another_patch(self):
        build.build(self.app, self.root / 'build.json')
        before = (self.app / 'Contents/Resources/app.asar').read_bytes()
        with self.assertRaisesRegex(RuntimeError, 'clean vendor'):
            build.build(self.app, self.root / 'another.json')
        self.assertEqual((self.app / 'Contents/Resources/app.asar').read_bytes(), before)

    def test_macos_verification_preserves_signature_gate(self):
        with patch.object(install.subprocess, 'run') as verifier:
            install.verify(self.app)
            self.assertEqual(verifier.call_count, 1)
            verifier.side_effect = subprocess.CalledProcessError(1, 'codesign')
            with self.assertRaises(subprocess.CalledProcessError):
                install.verify(self.app)

    def test_unsupported_composer_leaves_bundle_bytes_unchanged(self):
        target = self.root / 'Other/Teamwire.app'
        fixture(target, renderer=(ROOT / 'test/fixture.js').read_text().replace('getAssetFromFile', 'newAttachmentAPI'))
        before = {path.relative_to(target): path.read_bytes() for path in target.rglob('*') if path.is_file()}
        with self.assertRaisesRegex(RuntimeError, 'Compatibility check failed'):
            build.build(target, self.root / 'build.json')
        self.assertEqual(before, {path.relative_to(target): path.read_bytes() for path in target.rglob('*') if path.is_file()})

    def test_manifest_detects_modified_picker(self):
        build.build(self.app, self.root / 'build.json')
        archive, info, _ = build.load(self.app)
        packed = archive.replace({'build/gif-extension/picker.js': b'corrupt'})
        (self.app / 'Contents/Resources/app.asar').write_bytes(packed)
        info['ElectronAsarIntegrity']['Resources/app.asar']['hash'] = build.sha(build.Archive(packed).raw_header)
        (self.app / 'Contents/Info.plist').write_bytes(plistlib.dumps(info))
        with self.assertRaisesRegex(RuntimeError, 'modified or is incomplete'):
            build.inspect(self.app)

    def test_background_pass_waits_for_stability_then_app_exit(self):
        with patch.object(patcher, 'is_running', return_value=True), patch.object(build, 'plan') as plan:
            first = patcher.run_once(self.config, self.state, now=100)
            second = patcher.run_once(self.config, self.state, now=120)
        self.assertEqual(first['status'], 'waiting_stable')
        self.assertEqual(second['status'], 'waiting_exit')
        plan.assert_not_called()
        self.assertEqual(build.inspect(self.app)['kind'], 'vendor')

    def test_full_automatic_cycle_is_idempotent_and_retains_vendor(self):
        original = build.inspect(self.app)['asar_sha256']
        with patch.object(install, 'ensure_stopped'), patch.object(install, 'verify'), \
             patch.object(patcher, 'identity_available', return_value=True), patch.object(patcher.sign, 'sign'):
            first = patcher.run_once(self.config, self.state, force=True)
            second = patcher.run_once(self.config, self.state)
        self.assertEqual(first['status'], 'current', first)
        self.assertEqual(second['status'], 'current')
        self.assertFalse(second['changed'])
        backup = Path(first['original_backup'])
        self.assertEqual(build.inspect(backup)['asar_sha256'], original)
        self.assertTrue(Path(first['replaced_app']).exists())
        self.assertEqual(len(list((self.state / 'receipts').glob('*.json'))), 1)
        self.assertFalse((self.state / 'transaction.json').exists())

    def test_missing_signer_blocks_without_replacing_app(self):
        before = build.inspect(self.app)['asar_sha256']
        with patch.object(install, 'ensure_stopped'), patch.object(install, 'verify'), \
             patch.object(patcher, 'identity_available', return_value=False):
            result = patcher.run_once(self.config, self.state, force=True)
        self.assertEqual(result['status'], 'blocked')
        self.assertIn('signing identity', result['message'])
        self.assertEqual(build.inspect(self.app)['asar_sha256'], before)

    def test_bundle_change_during_preparation_aborts_replacement(self):
        source = self.root / 'Build/Teamwire.app'
        fixture(source)
        before = build.inspect(self.app)['asar_sha256']
        calls = []
        def guard():
            calls.append(1)
            if len(calls) == 2:
                raise RuntimeError('bundle changed')
        with patch.object(install, 'ensure_stopped'), patch.object(install, 'verify'):
            with self.assertRaisesRegex(RuntimeError, 'bundle changed'):
                install.install(source, self.app, self.state / 'backups', self.state / 'receipts', guard=guard)
        self.assertEqual(build.inspect(self.app)['asar_sha256'], before)
        self.assertFalse((self.state / 'backups').exists())

    def test_failed_post_install_check_rolls_back(self):
        source = self.root / 'Build/Teamwire.app'
        fixture(source, version='5.0.100')
        before = build.inspect(self.app)['asar_sha256']
        def postcheck(candidate):
            if candidate == self.app:
                raise RuntimeError('post-install check failed')
        with patch.object(install, 'ensure_stopped'), patch.object(install, 'verify'):
            with self.assertRaisesRegex(RuntimeError, 'post-install check failed'):
                install.install(source, self.app, self.state / 'backups', self.state / 'receipts',
                                postcheck=postcheck, journal=self.state / 'transaction.json')
        self.assertEqual(build.inspect(self.app)['asar_sha256'], before)
        self.assertFalse((self.state / 'transaction.json').exists())

    def test_recovery_restores_app_after_interrupted_rename(self):
        backup = self.state / 'backups/original.app'
        backup.parent.mkdir(parents=True)
        self.app.rename(backup)
        journal = self.state / 'transaction.json'
        install.atomic_json(journal, {'destination': str(self.app), 'backup': str(backup),
                                     'installed_asar_sha256': 'not installed'})
        with patch.object(install, 'ensure_stopped'), patch.object(install, 'verify'):
            self.assertTrue(install.recover(journal, self.app))
        self.assertTrue(self.app.exists())
        self.assertFalse(journal.exists())

    def test_status_uses_configured_app_in_custom_location(self):
        install.atomic_json(self.state / 'config.json', self.config)
        output = io.StringIO()
        with patch.object(sys, 'argv', ['patcher.py', 'status', '--state-dir', str(self.state)]), \
             patch.object(patcher, 'loaded', return_value=False), contextlib.redirect_stdout(output):
            self.assertEqual(patcher.main(), 0)
        result = json.loads(output.getvalue())
        self.assertEqual(result['app'], str(self.app))
        self.assertEqual(result['installed']['kind'], 'vendor')


if __name__ == '__main__':
    unittest.main()
