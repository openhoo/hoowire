#!/usr/bin/env python3
"""Create and verify deterministic, source-only release archives."""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import tarfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
PUBLIC_FILES = (
    'VERSION', 'README.md', 'LICENSE', 'NOTICE', 'CONTRIBUTING.md', 'SECURITY.md',
    'docs/compatibility.md',
    'scripts/build.py', 'scripts/install.py', 'scripts/patcher.py',
    'scripts/sign.py', 'scripts/transform.cjs', 'scripts/package.py',
    'src/main.cjs', 'src/preload.cjs', 'src/provider.cjs', 'src/picker.js',
    'test/fixture.js', 'test/provider.test.cjs', 'test/transform.test.cjs',
    'test/test_patcher.py',
    'vendor/acorn/acorn.cjs', 'vendor/acorn/LICENSE',
    'vendor/acorn/package.json', 'vendor/acorn/PROVENANCE.json',
)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def collect():
    files = {}
    for name in sorted(PUBLIC_FILES):
        path = ROOT / name
        if path.is_symlink() or not path.is_file():
            raise RuntimeError(f'Expected a regular release source file: {name}')
        files[name] = path.read_bytes()
    provenance = json.loads(files['vendor/acorn/PROVENANCE.json'])
    for name, expected in provenance['files'].items():
        if digest(files['vendor/acorn/' + name]) != expected:
            raise RuntimeError(f'Vendored Acorn checksum mismatch: {name}')
    return files


def package(output):
    files = collect()
    version = files['VERSION'].decode().strip()
    if not re.fullmatch(r'\d+\.\d+\.\d+', version):
        raise RuntimeError('VERSION must contain a numeric semantic version.')
    prefix = f'teamwire-gif-patcher-{version}'
    files['SOURCE-SHA256SUMS'] = ''.join(
        f'{digest(data)}  {name}\n' for name, data in files.items()).encode()
    output.mkdir(parents=True, exist_ok=True)
    zip_path = output / (prefix + '.zip')
    tar_path = output / (prefix + '.tar.gz')
    with zipfile.ZipFile(zip_path, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
        for name, data in files.items():
            info = zipfile.ZipInfo(prefix + '/' + name, date_time=(1980, 1, 1, 0, 0, 0))
            info.create_system = 3
            info.external_attr = 0o100644 << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, data)
    with tar_path.open('wb') as raw:
        with gzip.GzipFile(fileobj=raw, mode='wb', filename='', mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w', format=tarfile.USTAR_FORMAT) as archive:
                for name, data in files.items():
                    info = tarfile.TarInfo(prefix + '/' + name)
                    info.size = len(data)
                    info.mode = 0o644
                    archive.addfile(info, io.BytesIO(data))
    expected = {prefix + '/' + name: data for name, data in files.items()}
    with zipfile.ZipFile(zip_path) as archive:
        if len(archive.infolist()) != len(expected) or {
                item.filename: archive.read(item) for item in archive.infolist()} != expected:
            raise RuntimeError('ZIP contents differ from the release source allowlist.')
    with tarfile.open(tar_path, 'r:gz') as archive:
        members = archive.getmembers()
        if len(members) != len(expected) or any(not item.isfile() for item in members) or {
                item.name: archive.extractfile(item).read() for item in members} != expected:
            raise RuntimeError('TAR contents differ from the release source allowlist.')
    checksums = output / 'SHA256SUMS'
    checksums.write_text(''.join(f'{digest(path.read_bytes())}  {path.name}\n'
                                 for path in (zip_path, tar_path)))
    return {'version': version, 'source_files': len(files) - 1,
            'assets': [str(path) for path in (zip_path, tar_path, checksums)]}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT / 'dist')
    print(json.dumps(package(parser.parse_args().output), indent=2))
