#!/usr/bin/env python3
"""Discover and patch a compatible Teamwire app copy; never execute vendor JS."""
import argparse
import hashlib
from html.parser import HTMLParser
import json
from pathlib import Path
import plistlib
import posixpath
import shutil
import stat
import struct
import subprocess

import windows

ROOT = Path(__file__).resolve().parents[1]
RECIPE_VERSION = 2
EXTENSION_FILES = ('provider.cjs', 'main.cjs', 'preload.cjs', 'picker.js')
MANIFEST = 'build/gif-extension/manifest.json'
LEGACY_PATCHES = {
    'b1edaa915fd8997ede0f636b035d09c9487381f345a395505fd3245452f4b0aa':
    'bc523d053f8ee6216cdcde41b2c8922d1af203461e4d5630f4b06fa092dab029',
}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def extension_digest():
    return sha(b''.join(name.encode() + b'\0' + (ROOT / 'src' / name).read_bytes() + b'\0'
                        for name in EXTENSION_FILES))


def app_platform(app):
    mac = (app / 'Contents/Info.plist').is_file()
    win = (app / 'resources/app.asar').is_file() and (app / 'Teamwire.exe').is_file()
    if mac == win:
        raise RuntimeError('Expected one supported macOS or Windows Teamwire app layout.')
    return 'macos' if mac else 'windows'


def asar_path(app):
    return app / ('Contents/Resources/app.asar' if app_platform(app) == 'macos' else 'resources/app.asar')


def app_copy_name(app):
    return 'Teamwire.app' if app_platform(app) == 'macos' else 'Teamwire'


class Archive:
    def __init__(self, data):
        if len(data) < 16:
            raise RuntimeError('Truncated ASAR archive.')
        size_pickle, header_size, header_payload, json_size = struct.unpack('<IIII', data[:16])
        if size_pickle != 4 or header_size != header_payload + 4 or json_size > header_size - 8 or 8 + header_size > len(data):
            raise RuntimeError('Unsupported ASAR header.')
        self.data = data
        self.raw_header = data[16:16 + json_size]
        self.tree = json.loads(self.raw_header)
        self.payload = data[8 + header_size:]

    def node(self, path, create=False):
        if path.startswith('/') or any(part in ('', '.', '..') for part in path.split('/')):
            raise RuntimeError(f'Unsafe archive path: {path}')
        current = self.tree
        for part in path.split('/'):
            current = current.setdefault('files', {}).setdefault(part, {}) if create else current['files'][part]
        return current

    def has(self, path):
        try:
            self.node(path)
            return True
        except KeyError:
            return False

    def read(self, path):
        record = self.node(path)
        if 'link' in record or record.get('unpacked') or 'files' in record:
            raise RuntimeError(f'Expected a packed regular file: {path}')
        offset, size = int(record['offset']), record['size']
        if offset < 0 or size < 0 or offset + size > len(self.payload):
            raise RuntimeError(f'Invalid ASAR range: {path}')
        return self.payload[offset:offset + size]

    def replace(self, changes):
        offset = len(self.payload)
        appended = []
        for name, content in changes.items():
            record = self.node(name, create=True)
            record.clear()
            record.update({'size': len(content), 'offset': str(offset), 'integrity': {
                'algorithm': 'SHA256', 'hash': sha(content), 'blockSize': 4194304,
                'blocks': [sha(content[n:n + 4194304]) for n in range(0, len(content), 4194304)],
            }})
            offset += len(content)
            appended.append(content)
        raw_header = json.dumps(self.tree, separators=(',', ':'), ensure_ascii=False).encode()
        padded = (len(raw_header) + 3) & ~3
        header = struct.pack('<II', 4 + padded, len(raw_header)) + raw_header + bytes(padded - len(raw_header))
        return struct.pack('<II', 4, len(header)) + header + self.payload + b''.join(appended)


def load(app):
    platform = app_platform(app)
    if platform == 'macos':
        info = plistlib.loads((app / 'Contents/Info.plist').read_bytes())
        if info.get('CFBundleIdentifier') != 'eu.teamwire.app':
            raise RuntimeError('The selected app is not Teamwire.')
    else:
        info = {'platform': 'windows', 'fuses': windows.executable_fuses(app / 'Teamwire.exe')}
    archive = Archive(asar_path(app).read_bytes())
    package = json.loads(archive.read('package.json'))
    if package.get('name') != 'teamwire' or (platform == 'macos' and package.get('version') != info.get('CFBundleShortVersionString')):
        raise RuntimeError('Teamwire package and bundle metadata disagree.')
    if platform == 'macos':
        expected = info.get('ElectronAsarIntegrity', {}).get('Resources/app.asar', {})
        if expected.get('algorithm') != 'SHA256' or expected.get('hash') != sha(archive.raw_header):
            raise RuntimeError('ASAR header does not match the bundle integrity metadata.')
    return archive, info, package


def inspect(app):
    archive, _, package = load(app)
    fingerprint = sha(archive.data)
    platform = app_platform(app)
    if archive.has(MANIFEST):
        manifest = json.loads(archive.read(MANIFEST))
        if manifest.get('schema') != 1:
            raise RuntimeError('Unknown patch manifest schema.')
        for path, digest in manifest['patched_files'].items():
            if sha(archive.read(path)) != digest:
                raise RuntimeError(f'Installed patch was modified or is incomplete: {path}')
        current = manifest.get('extension_sha256') == extension_digest() and manifest.get('recipe') == RECIPE_VERSION
        return {'kind': 'patched' if current else 'extension_update', 'version': package['version'], 'platform': platform,
                'asar_sha256': fingerprint, 'original_asar_sha256': manifest['original_asar_sha256'],
                'manifest': manifest}
    if fingerprint in LEGACY_PATCHES:
        current = all(archive.read('build/gif-extension/' + name) == (ROOT / 'src' / name).read_bytes()
                      for name in EXTENSION_FILES)
        return {'kind': 'legacy_current' if current else 'extension_update', 'version': package['version'], 'platform': platform,
                'asar_sha256': fingerprint, 'original_asar_sha256': LEGACY_PATCHES[fingerprint]}
    if archive.has('build/gif-extension/picker.js'):
        raise RuntimeError('Unrecognized existing GIF patch. Restore its vendor backup before patching.')
    return {'kind': 'vendor', 'version': package['version'], 'platform': platform, 'asar_sha256': fingerprint,
            'original_asar_sha256': fingerprint}


def javascript(operation, source, node_bin=None, source_type='module'):
    node_bin = node_bin or shutil.which('node')
    if not node_bin:
        raise RuntimeError('Node.js is unavailable; configure an absolute node path.')
    result = subprocess.run([str(node_bin), str(ROOT / 'scripts/transform.cjs')],
                            input=json.dumps({'operation': operation, 'source': source, 'sourceType': source_type}),
                            encoding='utf-8', capture_output=True, timeout=90,
                            creationflags=windows.process_flags())
    if result.returncode:
        raise RuntimeError('Compatibility check failed: ' + result.stderr.strip())
    return json.loads(result.stdout)


class ModuleScripts(HTMLParser):
    def __init__(self):
        super().__init__()
        self.modules = []

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == 'script' and attrs.get('type') == 'module' and attrs.get('src'):
            self.modules.append((self.get_starttag_text(), attrs['src']))


def plan(app, node_bin=None):
    archive, _, package = load(app)
    if inspect(app)['kind'] != 'vendor':
        raise RuntimeError('Build from a clean vendor app, not an already patched copy.')
    main_path = package.get('main')
    if main_path != 'build/electron/main.js':
        raise RuntimeError('Teamwire Electron entry point changed; a new recipe is required.')
    html = archive.read('build/index.html').decode()
    parser = ModuleScripts()
    parser.feed(html)
    if len(parser.modules) != 1:
        raise RuntimeError('Expected one renderer module script.')
    module_tag, module_url = parser.modules[0]
    if not module_url.startswith('./assets/') or '?' in module_url or '#' in module_url or '..' in module_url.split('/'):
        raise RuntimeError('Unexpected renderer module path.')
    renderer_path = posixpath.normpath(posixpath.join('build', module_url))
    renderer = archive.read(renderer_path).decode()
    transformed = javascript('transform', renderer, node_bin)
    main = archive.read(main_path).decode()
    preload_path = 'build/electron/preloads/main.js'
    preload = archive.read(preload_path).decode()
    javascript('validate-main', main, node_bin, 'script')
    if 'contextBridge' not in preload or 'require("electron")' not in preload:
        raise RuntimeError('Main preload no longer exposes the supported context bridge.')
    if html.count(module_tag) != 1:
        raise RuntimeError('Ambiguous renderer script tag.')
    html = html.replace(module_tag, '<script src="./gif-extension/picker.js"></script>\n    ' + module_tag, 1)
    main += '\n// Local Teamwire GIF extension.\nrequire("../gif-extension/main.cjs");\n'
    preload += '\nrequire("../../gif-extension/preload.cjs");\n'
    javascript('validate', main, node_bin, 'script')
    javascript('validate', preload, node_bin, 'script')
    changes = {renderer_path: transformed['output'].encode(), 'build/index.html': html.encode(),
               main_path: main.encode(), preload_path: preload.encode()}
    for name in EXTENSION_FILES:
        content = (ROOT / 'src' / name).read_bytes()
        javascript('validate', content.decode(), node_bin, 'script')
        changes['build/gif-extension/' + name] = content
    manifest = {
        'schema': 1, 'recipe': RECIPE_VERSION, 'teamwire_version': package['version'],
        'original_asar_sha256': sha(archive.data), 'extension_sha256': extension_digest(),
        'renderer_path': renderer_path, 'probes': transformed['probes'],
        'original_files': {path: sha(archive.read(path)) for path in changes if archive.has(path)},
        'patched_files': {path: sha(content) for path, content in changes.items()},
    }
    changes[MANIFEST] = (json.dumps(manifest, indent=2) + '\n').encode()
    return archive, changes, manifest


def build(app, evidence_path=None, node_bin=None):
    app = app.resolve(strict=True)
    if app == Path('/Applications/Teamwire.app'):
        raise RuntimeError('Build a separate copy first; the installer performs replacement.')
    archive, changes, manifest = plan(app, node_bin)
    patched = archive.replace(changes)
    checked = Archive(patched)
    for path, expected in changes.items():
        if checked.read(path) != expected or checked.node(path)['integrity']['hash'] != sha(expected):
            raise RuntimeError(f'Archive verification failed: {path}')
    archive_path = asar_path(app)
    platform = app_platform(app)
    original_mode = stat.S_IMODE(archive_path.stat().st_mode)
    try:
        # MSI files can carry Windows' read-only attribute into the staged copy.
        # Restore that attribute after replacing this copy's archive bytes.
        if platform == 'windows':
            archive_path.chmod(original_mode | stat.S_IWRITE)
        archive_path.write_bytes(patched)
    finally:
        if platform == 'windows':
            archive_path.chmod(original_mode)
    if app_platform(app) == 'macos':
        plist_path = app / 'Contents/Info.plist'
        info = plistlib.loads(plist_path.read_bytes())
        info['ElectronAsarIntegrity'] = {'Resources/app.asar': {'algorithm': 'SHA256', 'hash': sha(checked.raw_header)}}
        plist_path.write_bytes(plistlib.dumps(info, sort_keys=False))
    if inspect(app)['kind'] != 'patched':
        raise RuntimeError('Final patch manifest verification failed.')
    result = {'app': str(app), 'teamwire_version': manifest['teamwire_version'], 'recipe': RECIPE_VERSION,
              'original_asar_sha256': manifest['original_asar_sha256'], 'patched_asar_sha256': sha(patched),
              'asar_header_sha256': sha(checked.raw_header), 'changed_files': list(changes),
              'probes': manifest['probes']}
    evidence_path = evidence_path or ROOT / 'evidence/build.json'
    evidence_path.parent.mkdir(parents=True, exist_ok=True)
    evidence_path.write_text(json.dumps(result, indent=2) + '\n')
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('app', type=Path)
    parser.add_argument('--node', type=Path)
    parser.add_argument('--check', action='store_true', help='Check compatibility without changing any files')
    args = parser.parse_args()
    if args.check:
        _, changes, manifest = plan(args.app, args.node)
        print(json.dumps({'compatible': True, 'manifest': manifest, 'changed_files': list(changes)}, indent=2))
    else:
        print(json.dumps(build(args.app, node_bin=args.node), indent=2))
