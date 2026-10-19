#!/usr/bin/env python3
"""Sign a patched app and its native code with one local development identity."""
import argparse
from pathlib import Path
import subprocess

import build
import windows

MACHO_MAGICS = {
    b'\xfe\xed\xfa\xce', b'\xce\xfa\xed\xfe',
    b'\xfe\xed\xfa\xcf', b'\xcf\xfa\xed\xfe',
    b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca',
    b'\xca\xfe\xba\xbf', b'\xbf\xba\xfe\xca',
}


def run(*args):
    subprocess.run(args, check=True)


def sign(app, identity):
    app = app.resolve(strict=True)
    if build.app_platform(app) == 'windows':
        # The supported Windows recipe edits only app.asar. The PE and its
        # Authenticode signature remain byte-for-byte unchanged.
        windows.executable_fuses(app / 'Teamwire.exe')
        windows.verify_app(app)
        return
    if app == Path('/Applications/Teamwire.app'):
        raise RuntimeError('Sign a separate app copy, then install it.')
    if not identity:
        raise RuntimeError('An Apple Development identity is required for macOS signing.')
    binaries = []
    for candidate in app.rglob('*'):
        if candidate.is_symlink() or not candidate.is_file():
            continue
        with candidate.open('rb') as stream:
            if stream.read(4) in MACHO_MAGICS:
                binaries.append(candidate)
    if not binaries:
        raise RuntimeError('No Mach-O binaries found in the app.')
    common = ['codesign', '--force', '--sign', identity,
              '--preserve-metadata=entitlements,flags,runtime', '--timestamp=none']
    for candidate in sorted(binaries, key=lambda item: len(item.parts), reverse=True):
        run(*common, str(candidate))
    run(*common, '--deep', str(app))
    run('codesign', '--verify', '--deep', '--strict', str(app))
    print(f'Signed {len(binaries)} native binaries and verified {app}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('app', type=Path)
    parser.add_argument('--identity', help='Local macOS codesigning identity or SHA-1 fingerprint')
    args = parser.parse_args()
    sign(args.app, args.identity)
