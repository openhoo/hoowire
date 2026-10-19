#!/usr/bin/env python3
"""Automatically restore the GIF picker after compatible Teamwire updates."""
import argparse
from datetime import datetime
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time

import build
import install
import sign

ROOT = Path(__file__).resolve().parents[1]
SUPPORT = Path.home() / 'Library/Application Support/Teamwire GIF Patcher'
LABEL = 'eu.openhoo.teamwire-gif-patcher'
PLIST = Path.home() / 'Library/LaunchAgents' / (LABEL + '.plist')


def read_json(path, default=None):
    return json.loads(path.read_text()) if path.exists() else (default if default is not None else {})


def stamp(app):
    paths = [app, app / 'Contents/Resources/app.asar', app / 'Contents/Info.plist',
             app / 'Contents/MacOS/Teamwire', app / 'Contents/_CodeSignature/CodeResources']
    values = []
    for path in paths:
        value = path.stat()
        values.append([str(path), value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns])
    return build.sha(json.dumps(values).encode())


def is_running():
    try:
        install.ensure_stopped()
        return False
    except RuntimeError:
        return True


def identity_available(identity):
    result = subprocess.run(['security', 'find-identity', '-v', '-p', 'codesigning'],
                            text=True, capture_output=True, check=True)
    return identity in result.stdout


def discover_identity():
    result = subprocess.run(['security', 'find-identity', '-v', '-p', 'codesigning'],
                            text=True, capture_output=True, check=True)
    identities = re.findall(r'\b([A-F0-9]{40}) "Apple Development:[^"]+"', result.stdout)
    if len(identities) != 1:
        raise RuntimeError('Specify --identity with one local Apple Development signing identity.')
    return identities[0]


def verified_vendor(app, fingerprint):
    details = build.inspect(app)
    if details['kind'] != 'vendor' or details['asar_sha256'] != fingerprint:
        raise RuntimeError('Original backup is not the expected unmodified vendor archive.')
    install.verify(app)


def save_vendor(source, details, state_dir):
    fingerprint = details['original_asar_sha256']
    name = f'vendor-{fingerprint}.app'
    target = state_dir / 'backups' / name
    if target.exists():
        verified_vendor(target, fingerprint)
        return target
    verified_vendor(source, fingerprint)
    target.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.vendor-', dir=target.parent) as temporary:
        staged = Path(temporary) / 'Teamwire.app'
        subprocess.run(['ditto', str(source), str(staged)], check=True)
        verified_vendor(staged, fingerprint)
        staged.rename(target)
    return target


def original_for(details, state_dir):
    candidate = state_dir / 'backups' / f'vendor-{details["original_asar_sha256"]}.app'
    if not candidate.exists():
        raise RuntimeError('The original vendor backup for this patch is missing.')
    verified_vendor(candidate, details['original_asar_sha256'])
    return candidate


def run_once(config, state_dir, force=False, rebuild=False, now=None):
    """One bounded pass. It never quits or launches Teamwire."""
    now = time.time() if now is None else now
    state_dir.mkdir(parents=True, exist_ok=True)
    with (state_dir / 'patcher.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return {'status': 'busy', 'message': 'Another patcher pass is active.'}
        previous = read_json(state_dir / 'state.json')
        app = Path(config['app'])
        state = dict(previous)

        def report(status, message, **extra):
            state.update(status=status, message=message, checked_at=datetime.now().astimezone().isoformat(), **extra)
            changed = any(state.get(key) != previous.get(key) for key in ('status', 'message', 'observed', 'extension'))
            install.atomic_json(state_dir / 'state.json', state)
            if changed:
                with (state_dir / 'activity.log').open('a') as log:
                    log.write(json.dumps({key: state.get(key) for key in ('checked_at', 'status', 'message', 'version')}) + '\n')
            return dict(state, changed=changed)

        try:
            journal = state_dir / 'transaction.json'
            if journal.exists():
                if is_running():
                    return report('waiting_exit', 'Teamwire must close before interrupted-install recovery.')
                install.recover(journal, app)
            if not app.exists():
                return report('waiting_app', 'Waiting for Teamwire to be installed.')
            observed = stamp(app)
            extension = f'{build.RECIPE_VERSION}:{build.extension_digest()}'
            same = previous.get('observed') == observed and previous.get('extension') == extension
            state.update(observed=observed, extension=extension)
            cached = previous.get('status') == 'current' or (
                previous.get('status') == 'blocked' and now < previous.get('retry_after', 0))
            if same and cached and not force and not rebuild:
                return dict(previous, changed=False)
            details = build.inspect(app)
            state.update(version=details['version'], asar_sha256=details['asar_sha256'])
            if details['kind'] in ('patched', 'legacy_current') and not rebuild:
                install.verify(app)
                return report('current', 'GIF picker is installed and current.')
            if not same:
                state['first_seen'] = now
            if not force and now - state.get('first_seen', now) < config.get('settle_seconds', 15):
                return report('waiting_stable', 'Waiting for the Teamwire update to finish.')
            if is_running():
                return report('waiting_exit', 'A patch is pending; it will apply after Teamwire closes.')
            install.verify(app)
            if not identity_available(config['identity']):
                raise RuntimeError('The configured local signing identity is unavailable. Re-enable with a valid identity.')
            if details['kind'] == 'vendor':
                # Prove compatibility before creating backups or changing the app.
                build.plan(app, config['node'])
                if stamp(app) != observed or is_running():
                    return report('waiting_stable', 'Teamwire changed during compatibility checking; retrying later.')
                source = save_vendor(app, details, state_dir)
            else:
                source = original_for(details, state_dir)
            work = state_dir / 'work'
            work.mkdir(exist_ok=True)
            with tempfile.TemporaryDirectory(prefix='patch-', dir=work) as temporary:
                staged = Path(temporary) / 'Teamwire.app'
                subprocess.run(['ditto', str(source), str(staged)], check=True)
                build_result = build.build(staged, state_dir / 'last-build.json', config['node'])
                sign.sign(staged, config['identity'])

                def guard():
                    install.ensure_stopped()
                    if stamp(app) != observed:
                        raise RuntimeError('Teamwire changed while the patch was being prepared; retry after it settles.')

                def postcheck(candidate):
                    actual = build.inspect(candidate)
                    if actual['kind'] != 'patched' or actual['asar_sha256'] != build_result['patched_asar_sha256']:
                        raise RuntimeError('Installed patch does not match the verified build.')

                result = install.install(staged, app, backup_root=state_dir / 'backups',
                                         evidence_dir=state_dir / 'receipts', guard=guard,
                                         postcheck=postcheck, journal=journal)
            state.update(observed=stamp(app), asar_sha256=result['asar_sha256'],
                         original_backup=str(source), replaced_app=result['retained_previous_app'])
            return report('current', 'GIF picker patched, signed, installed and verified.')
        except Exception as error:
            return report('blocked', str(error), retry_after=now + config.get('retry_seconds', 900))


def service_target():
    return f'gui/{os.getuid()}/{LABEL}'


def loaded():
    return subprocess.run(['launchctl', 'print', service_target()], capture_output=True).returncode == 0


def disable_service():
    if loaded():
        subprocess.run(['launchctl', 'bootout', service_target()], check=True)


def enable(args):
    import plistlib
    state_dir = args.state_dir.absolute()
    state_dir.mkdir(parents=True, exist_ok=True)
    state_dir.chmod(0o700)
    old = read_json(state_dir / 'config.json')
    node = str(args.node or old.get('node') or shutil.which('node') or '')
    if not node or not Path(node).is_file():
        raise RuntimeError('Specify an installed Node.js executable with --node.')
    identity = args.identity or old.get('identity')
    if not identity or (not args.identity and not identity_available(identity)):
        identity = discover_identity()
    if not identity_available(identity):
        raise RuntimeError('Selected signing identity is unavailable.')
    config = {'app': str(args.app.absolute()), 'node': str(Path(node).absolute()), 'identity': identity,
              'settle_seconds': 15, 'interval_seconds': 120, 'retry_seconds': 900, 'enabled': True}
    # Capture the existing legacy patch's vendor backup during the one-time setup.
    details = build.inspect(args.app)
    if details['kind'] != 'vendor' and not (state_dir / 'backups' / f'vendor-{details["original_asar_sha256"]}.app').exists():
        candidates = sorted((ROOT / 'backups').glob('*.app')) if (ROOT / 'backups').exists() else []
        for candidate in candidates:
            if build.inspect(candidate)['asar_sha256'] == details['original_asar_sha256']:
                save_vendor(candidate, details, state_dir)
                break
        else:
            raise RuntimeError('Could not find the original vendor app; keep its backup before enabling.')
    disable_service()
    runtime = state_dir / 'runtime'
    if ROOT != runtime:
        with tempfile.TemporaryDirectory(prefix='.runtime-', dir=state_dir) as temporary:
            prepared = Path(temporary) / 'runtime'
            for name in ('scripts', 'src', 'vendor'):
                shutil.copytree(ROOT / name, prepared / name, ignore=shutil.ignore_patterns('__pycache__', '*.pyc'))
            shutil.copy2(ROOT / 'README.md', prepared / 'README.md')
            previous = state_dir / 'runtime.previous'
            if previous.exists():
                shutil.rmtree(previous)
            if runtime.exists():
                runtime.rename(previous)
            prepared.rename(runtime)
    install.atomic_json(state_dir / 'config.json', config)
    agent = {
        'Label': LABEL,
        'ProgramArguments': ['/usr/bin/python3', str(runtime / 'scripts/patcher.py'), 'check', '--quiet',
                             '--state-dir', str(state_dir)],
        'RunAtLoad': True, 'StartInterval': config['interval_seconds'],
        'WatchPaths': [str(args.app / 'Contents/Resources'), str(args.app / 'Contents/Info.plist')],
        'ProcessType': 'Background',
        'EnvironmentVariables': {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'PYTHONDONTWRITEBYTECODE': '1'},
        'StandardOutPath': str(state_dir / 'agent.log'), 'StandardErrorPath': str(state_dir / 'agent-error.log'),
    }
    PLIST.parent.mkdir(parents=True, exist_ok=True)
    PLIST.write_bytes(plistlib.dumps(agent))
    PLIST.chmod(0o600)
    subprocess.run(['plutil', '-lint', str(PLIST)], check=True)
    subprocess.run(['launchctl', 'enable', service_target()], check=True)
    subprocess.run(['launchctl', 'bootstrap', f'gui/{os.getuid()}', str(PLIST)], check=True)
    return {'enabled': loaded(), 'agent': str(PLIST), 'runtime': str(runtime),
            'app': config['app'], 'interval_seconds': config['interval_seconds']}


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('check', 'apply', 'status', 'enable', 'disable'))
    parser.add_argument('--app', type=Path, help='Teamwire location for enable or status')
    parser.add_argument('--state-dir', type=Path, default=SUPPORT)
    parser.add_argument('--node', type=Path)
    parser.add_argument('--identity')
    parser.add_argument('--rebuild', action='store_true', help='Rebuild a current patch from its vendor backup')
    parser.add_argument('--quiet', action='store_true', help='Print only status changes')
    args = parser.parse_args()
    try:
        if args.command == 'enable':
            args.app = args.app or Path('/Applications/Teamwire.app')
            result = enable(args)
        elif args.command == 'disable':
            disable_service()
            PLIST.unlink(missing_ok=True)
            config = read_json(args.state_dir / 'config.json')
            config['enabled'] = False
            install.atomic_json(args.state_dir / 'config.json', config)
            result = {'enabled': loaded(), 'backups_retained': str(args.state_dir / 'backups')}
        elif args.command == 'status':
            config = read_json(args.state_dir / 'config.json')
            app = args.app or Path(config.get('app', '/Applications/Teamwire.app'))
            result = {'enabled': loaded(), 'app': str(app),
                      'installed': build.inspect(app), 'worker': read_json(args.state_dir / 'state.json'),
                      'state_directory': str(args.state_dir)}
            result['installed'].pop('manifest', None)
        else:
            config = read_json(args.state_dir / 'config.json')
            if not config:
                raise RuntimeError('Enable the patcher first, or create a config.json for a test app.')
            if args.command == 'check' and not config.get('enabled', True):
                return 0
            result = run_once(config, args.state_dir, force=args.command == 'apply', rebuild=args.rebuild)
        if not args.quiet or result.get('changed'):
            print(json.dumps(result, indent=2))
        return 1 if result.get('status') == 'blocked' else 0
    except Exception as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
