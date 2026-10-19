#!/usr/bin/env python3
"""Install or restore a verified Teamwire copy, retaining the replaced app."""
import argparse
from contextlib import contextmanager
from datetime import datetime
import fcntl
import hashlib
import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def app_version(app):
    return plistlib.loads((app / 'Contents/Info.plist').read_bytes())['CFBundleShortVersionString']


def verify(app):
    info = plistlib.loads((app / 'Contents/Info.plist').read_bytes())
    if info['CFBundleIdentifier'] != 'eu.teamwire.app':
        raise RuntimeError('Source is not Teamwire.')
    subprocess.run(['codesign', '--verify', '--deep', '--strict', str(app)], check=True)


def ensure_stopped():
    processes = subprocess.check_output(['ps', '-axo', 'pid=,comm='], text=True)
    if any('/Teamwire.app/Contents/' in line or line.rstrip().endswith('/Contents/MacOS/Teamwire')
           for line in processes.splitlines()):
        raise RuntimeError('Quit Teamwire before installing so its profile has only one writer.')


def atomic_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.chmod(0o600)
    temporary.replace(path)


@contextmanager
def destination_lock(destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    with (destination.parent / '.teamwire-gif-install.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError('Another Teamwire install or recovery is active.')
        yield


def recover(journal, destination):
    with destination_lock(destination):
        return _recover(journal, destination)


def _recover(journal, destination):
    """Recover the rename gap after a power loss or terminated patcher."""
    if not journal.exists():
        return False
    ensure_stopped()
    record = json.loads(journal.read_text())
    if Path(record['destination']) != destination.absolute():
        raise RuntimeError('Recovery journal belongs to another app destination.')
    backup = Path(record['backup'])
    if destination.exists():
        try:
            verify(destination)
            digest = hashlib.sha256((destination / 'Contents/Resources/app.asar').read_bytes()).hexdigest()
            if digest == record['installed_asar_sha256']:
                journal.unlink()
                return True
        except (subprocess.CalledProcessError, OSError):
            pass
        if backup.exists():
            verify(backup)
            failed = backup.with_name(backup.name + '.failed-install')
            if failed.exists():
                raise RuntimeError(f'Recovery requires inspection: {failed}')
            destination.rename(failed)
            shutil.move(str(backup), str(destination))
        else:
            # A crash before the first rename leaves the original app in place.
            verify(destination)
    elif backup.exists():
        verify(backup)
        shutil.move(str(backup), str(destination))
    else:
        raise RuntimeError('Both the app and its recovery backup are missing.')
    verify(destination)
    journal.unlink()
    return True


def install(source, destination, backup_root=None, evidence_dir=None, **options):
    with destination_lock(destination):
        return _install(source, destination, backup_root, evidence_dir, **options)


def _install(source, destination, backup_root=None, evidence_dir=None, guard=None, postcheck=None, journal=None):
    source = source.resolve(strict=True)
    destination = destination.absolute()
    if source == destination.resolve():
        raise RuntimeError('Source and destination must differ.')
    ensure_stopped()
    if guard:
        guard()
    verify(source)
    destination.parent.mkdir(parents=True, exist_ok=True)
    timestamp = datetime.now().strftime('%Y%m%d-%H%M%S-%f')
    backup_root = backup_root or ROOT / 'backups'
    evidence_dir = evidence_dir or ROOT / 'evidence'
    backup = backup_root / f'Teamwire-before-install-{timestamp}.app'
    if backup.exists():
        raise RuntimeError(f'Backup path already exists: {backup}')
    temporary = Path(tempfile.mkdtemp(prefix='.teamwire-gif-', dir=destination.parent))
    retained = False
    try:
        staged = Path(temporary) / 'Teamwire.app'
        subprocess.run(['ditto', str(source), str(staged)], check=True)
        verify(staged)
        if postcheck:
            postcheck(staged)
        ensure_stopped()
        if guard:
            guard()
        if journal:
            if journal.exists():
                raise RuntimeError('A previous installation requires recovery first.')
            atomic_json(journal, {'destination': str(destination), 'backup': str(backup),
                                 'staged': str(staged), 'installed_asar_sha256': hashlib.sha256(
                                     (staged / 'Contents/Resources/app.asar').read_bytes()).hexdigest()})
        if destination.exists():
            backup.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(destination), str(backup))
            retained = True
        try:
            staged.rename(destination)
            verify(destination)
            if postcheck:
                postcheck(destination)
        except BaseException:
            if destination.exists():
                shutil.move(str(destination), str(Path(temporary) / 'failed-install.app'))
            if retained:
                shutil.move(str(backup), str(destination))
                verify(destination)
            if journal:
                journal.unlink(missing_ok=True)
            raise
        if journal:
            journal.unlink(missing_ok=True)
    finally:
        if not journal or not journal.exists():
            shutil.rmtree(temporary, ignore_errors=True)
    result = {
        'installed_at': datetime.now().astimezone().isoformat(),
        'source': str(source), 'destination': str(destination),
        'version': app_version(destination),
        'asar_sha256': hashlib.sha256((destination / 'Contents/Resources/app.asar').read_bytes()).hexdigest(),
        'retained_previous_app': str(backup) if retained else None,
    }
    atomic_json(evidence_dir / f'install-{timestamp}.json', result)
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=ROOT / 'build/Teamwire.app')
    parser.add_argument('--destination', type=Path, default=Path('/Applications/Teamwire.app'))
    args = parser.parse_args()
    print(json.dumps(install(args.source, args.destination), indent=2))
