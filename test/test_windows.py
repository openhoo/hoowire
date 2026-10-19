import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from test_patcher import fixture

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
import build
import install
import patcher
import windows


def fixture_windows(app, states=b'10110001'):
    fixture(app)
    (app / 'Contents/Resources').rename(app / 'resources')
    shutil.rmtree(app / 'Contents')
    executable = bytearray(128)
    executable[:2] = b'MZ'
    struct.pack_into('<I', executable, 0x3c, 64)
    executable[64:68] = b'PE\0\0'
    executable += windows.FUSE_SENTINEL + bytes([1, len(states)]) + states
    (app / 'Teamwire.exe').write_bytes(executable)


class WindowsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.app = self.root / 'Programs/Teamwire ü with spaces'
        fixture_windows(self.app)
        self.state = self.root / 'state'
        self.config = {'app': str(self.app), 'node': shutil.which('node'), 'identity': None}

    def tearDown(self):
        self.temp.cleanup()

    def test_windows_build_preserves_executable_and_discovers_composer(self):
        before = (self.app / 'Teamwire.exe').read_bytes()
        result = build.build(self.app, self.root / 'build.json')
        actual = build.inspect(self.app)
        self.assertEqual(actual['platform'], 'windows')
        self.assertEqual(actual['kind'], 'patched')
        self.assertEqual(actual['asar_sha256'], result['patched_asar_sha256'])
        self.assertEqual(before, (self.app / 'Teamwire.exe').read_bytes())

    def test_integrity_enabled_binary_is_rejected_without_modification(self):
        executable = self.app / 'Teamwire.exe'
        executable.write_bytes(executable.read_bytes().replace(b'10110001', b'10111001'))
        before = {p.relative_to(self.app): p.read_bytes() for p in self.app.rglob('*') if p.is_file()}
        with self.assertRaisesRegex(RuntimeError, 'embedded ASAR integrity'):
            build.build(self.app, self.root / 'build.json')
        self.assertEqual(before, {p.relative_to(self.app): p.read_bytes() for p in self.app.rglob('*') if p.is_file()})

    def test_unknown_or_ambiguous_fuse_wire_is_rejected(self):
        executable = self.app / 'Teamwire.exe'
        before = executable.read_bytes()
        for value in (before.replace(b'10110001', b'1011r001'), before + before[128:]):
            executable.write_bytes(value)
            with self.assertRaises(RuntimeError):
                windows.executable_fuses(executable)

    def test_worker_retains_backup_and_needs_no_apple_identity(self):
        original = build.asar_path(self.app).read_bytes()
        executable = (self.app / 'Teamwire.exe').read_bytes()
        with patch.object(install, 'ensure_stopped'), \
             patch.object(windows, 'verify_app', return_value={'version': '5.0.99.1'}), \
             patch.object(patcher, 'identity_available', side_effect=AssertionError('No Apple keychain on Windows')):
            first = patcher.run_once(self.config, self.state, force=True)
            second = patcher.run_once(self.config, self.state)
        self.assertEqual(first['status'], 'current', first)
        self.assertFalse(second['changed'])
        self.assertEqual(build.asar_path(Path(first['original_backup'])).read_bytes(), original)
        self.assertEqual((self.app / 'Teamwire.exe').read_bytes(), executable)
        self.assertFalse((self.state / 'transaction.json').exists())

    def test_windows_postcheck_failure_rolls_back(self):
        source = self.root / 'build/Teamwire'
        fixture_windows(source)
        build.build(source, self.root / 'build.json')
        before = build.asar_path(self.app).read_bytes()
        def postcheck(candidate):
            if candidate == self.app:
                raise RuntimeError('postcheck failed')
        with patch.object(install, 'ensure_stopped'), \
             patch.object(windows, 'verify_app', return_value={'version': '5.0.99.1'}):
            with self.assertRaisesRegex(RuntimeError, 'postcheck failed'):
                install.install(source, self.app, self.state / 'backups', self.state / 'receipts',
                                postcheck=postcheck, journal=self.state / 'transaction.json')
        self.assertEqual(build.asar_path(self.app).read_bytes(), before)

    def test_windows_interrupted_install_restores_original(self):
        backup = self.state / 'backups/vendor'
        backup.parent.mkdir(parents=True)
        self.app.rename(backup)
        journal = self.state / 'transaction.json'
        install.atomic_json(journal, {'destination': str(self.app), 'backup': str(backup),
                                     'installed_asar_sha256': 'not installed'})
        with patch.object(install, 'ensure_stopped'), \
             patch.object(windows, 'verify_app', return_value={'version': '5.0.99.1'}):
            self.assertTrue(install.recover(journal, self.app))
        self.assertEqual(build.inspect(self.app)['kind'], 'vendor')

    def test_cross_process_lock_is_exclusive_and_released_on_close(self):
        lock_path = self.root / 'exclusive.lock'
        child = """
import sys
sys.path.insert(0, sys.argv[1])
import install
with open(sys.argv[2], 'a+b') as stream:
    try:
        install.lock_file(stream)
    except BlockingIOError:
        raise SystemExit(23)
"""
        command = [sys.executable, '-c', child, str(ROOT / 'scripts'), str(lock_path)]
        with lock_path.open('a+b') as stream:
            install.lock_file(stream)
            self.assertEqual(subprocess.run(command).returncode, 23)
        self.assertEqual(subprocess.run(command).returncode, 0)

    def test_powershell_parameters_are_json_not_source(self):
        value = "C:\\Teamwire ü\\a'; throw 'injected"
        with patch.object(windows.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, '{}', '')) as run:
            windows.powershell('$payload.path | ConvertTo-Json', {'path': value})
        args, kwargs = run.call_args
        self.assertNotIn(value, args[0][-1])
        self.assertEqual(json.loads(kwargs['input'])['path'], value)
        self.assertEqual(kwargs['encoding'], 'utf-8')

    def test_signed_version_must_match_archive(self):
        with patch.object(windows, 'verify_app', return_value={'version': '99.0.0.1'}):
            with self.assertRaisesRegex(RuntimeError, 'versions disagree'):
                install.verify(self.app)

    @unittest.skipUnless(os.name == 'nt', 'Requires native Windows Authenticode')
    def test_unsigned_executable_is_rejected_by_real_windows_verifier(self):
        with self.assertRaises(RuntimeError):
            windows.verify_app(self.app)


if __name__ == '__main__':
    unittest.main()
