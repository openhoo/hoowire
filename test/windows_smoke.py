"""Opt-in native Windows CI qualification using the pinned official vendor MSI."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
import build
import install
import patcher
import windows

MSI_URL = 'https://desktop.teamwire.eu/dist/v5.0.1/teamwire-5.0.1-win-x64.msi'
MSI_SHA256 = '0ce660fde43034eac59604eee3b63dbac480bc2087b8d729cd9a93b6590271b3'


def stop(process):
    subprocess.run(['taskkill.exe', '/PID', str(process.pid), '/T', '/F'], capture_output=True)
    process.wait(timeout=30)
    for _ in range(40):
        try:
            windows.ensure_stopped()
            return
        except RuntimeError:
            time.sleep(0.25)
    raise RuntimeError('The isolated Windows app did not stop.')


def main():
    if os.name != 'nt' or os.environ.get('GITHUB_ACTIONS') != 'true':
        raise RuntimeError('This probe is restricted to disposable native Windows GitHub runners.')
    root = Path(os.environ['RUNNER_TEMP']) / 'Hoowire Windows ü smoke'
    root.mkdir()
    installer = root / 'Teamwire.msi'
    with urllib.request.urlopen(MSI_URL, timeout=60) as response, installer.open('wb') as stream:
        shutil.copyfileobj(response, stream)
    if hashlib.sha256(installer.read_bytes()).hexdigest() != MSI_SHA256:
        raise RuntimeError('Official MSI checksum differs from the reviewed vendor installer.')
    windows.powershell("""
$signature = Get-AuthenticodeSignature -LiteralPath $payload.path
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'CN=Teamwire GmbH') {
    throw 'The pinned vendor MSI does not have the expected valid publisher signature.'
}
""", {'path': str(installer)})
    extracted = root / 'vendor'
    installer_log = root / 'msi.log'
    try:
        result = subprocess.run(['msiexec.exe', '/a', str(installer), '/qn', 'TARGETDIR=' + str(extracted),
                                 '/L*v', str(installer_log)], timeout=300)
        if result.returncode not in (0, 3010):
            raise RuntimeError(f'MSI administrative extraction failed: {result.returncode}')
    except (subprocess.TimeoutExpired, RuntimeError):
        if installer_log.is_file():
            # Installer diagnostics only, from a disposable runner; no app profile.
            lines = installer_log.read_text(encoding='utf-16', errors='replace').splitlines()[-60:]
            print('\n'.join(lines).replace(str(root), '<smoke-root>'), flush=True)
        raise
    apps = [p.parent for p in extracted.rglob('Teamwire.exe') if (p.parent / 'resources/app.asar').is_file()]
    if len(apps) != 1:
        raise RuntimeError('The MSI did not contain one supported Teamwire app.')
    app = apps[0]
    original = build.inspect(app)
    install.verify(app)
    executable_hash = hashlib.sha256((app / 'Teamwire.exe').read_bytes()).hexdigest()
    state = root / 'state ü with spaces'
    patcher_command = [sys.executable, str(ROOT / 'scripts/patcher.py')]
    def cli(command, *args):
        process = subprocess.run(patcher_command + [command, '--state-dir', str(state), *args],
                                 capture_output=True, encoding='utf-8', check=True)
        return json.loads(process.stdout)
    # Avoid inheriting a developer profile. The probe never authenticates or sends.
    app_env = dict(os.environ, LOCALAPPDATA=str(root / 'empty-profile'))
    (root / 'empty-profile').mkdir()
    process = None
    try:
        enabled = cli('enable', '--app', str(app), '--node', shutil.which('node'))
        if not enabled['enabled']:
            raise RuntimeError('The Windows scheduled task was not enabled.')
        process = subprocess.Popen([str(app / 'Teamwire.exe')], env=app_env,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(60):
            if patcher.is_running():
                break
            time.sleep(0.25)
        waiting = cli('apply')
        if waiting.get('status') != 'waiting_exit':
            raise RuntimeError(f'Expected app-exit deferral, got {waiting}')
        stop(process)
        process = None
        applied = cli('apply')
        if applied.get('status') != 'current':
            raise RuntimeError(f'Windows patch did not install: {applied}')
        if cli('check').get('changed') is not False:
            raise RuntimeError('The Windows second pass was not unchanged.')
        if hashlib.sha256((app / 'Teamwire.exe').read_bytes()).hexdigest() != executable_hash:
            raise RuntimeError('The vendor executable was modified.')
        install.verify(app)
        with socket.socket() as server:
            server.bind(('127.0.0.1', 0))
            port = server.getsockname()[1]
        process = subprocess.Popen([str(app / 'Teamwire.exe'), f'--remote-debugging-port={port}'], env=app_env,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        native = subprocess.run(['node', str(ROOT / 'test/windows_runtime.cjs'), str(port)],
                                capture_output=True, encoding='utf-8', timeout=150)
        if native.returncode:
            raise RuntimeError(native.stderr or native.stdout)
        runtime = json.loads(native.stdout)
        stop(process)
        process = None
        # A real scheduler pass must execute the installed standalone runtime.
        before = json.loads((state / 'state.json').read_text())['checked_at']
        # Make the next check record a fresh result rather than its cached no-op.
        status = json.loads((state / 'state.json').read_text())
        status['extension'] = 'force-native-scheduler-check'
        install.atomic_json(state / 'state.json', status)
        windows.powershell(windows.TASK_PREFIX + 'Start-ScheduledTask -TaskPath $taskPath -TaskName $taskName')
        scheduled = False
        for _ in range(80):
            latest = json.loads((state / 'state.json').read_text())
            if latest.get('checked_at') != before and latest.get('status') == 'current':
                scheduled = True
                break
            time.sleep(0.5)
        if not scheduled:
            info = windows.powershell(windows.TASK_PREFIX + '(Get-ScheduledTaskInfo -TaskName $taskName -TaskPath $taskPath) | ConvertTo-Json -Compress')
            raise RuntimeError('The Windows scheduled runtime did not complete: ' + info)
        restored = install.install(Path(applied['original_backup']), app, state / 'backups', state / 'receipts')
        if restored['asar_sha256'] != original['asar_sha256']:
            raise RuntimeError('The Windows restore did not recover the original vendor archive.')
        record = {'vendor_version': original['version'], 'vendor_msi_sha256': MSI_SHA256,
                  'authenticode_valid': True, 'executable_unchanged': True, 'waited_for_exit': True,
                  'patch_and_restore': True, 'scheduled_runtime': scheduled, 'runtime': runtime}
        print(json.dumps(record, indent=2))
        summary = os.environ.get('GITHUB_STEP_SUMMARY')
        if summary:
            with open(summary, 'a', encoding='utf-8') as stream:
                stream.write('## Native Windows vendor verification\n\n```json\n' + json.dumps(record, indent=2) + '\n```\n')
    finally:
        if process is not None:
            stop(process)
        cli('disable')


if __name__ == '__main__':
    main()
