"""Windows discovery, executable checks, and the per-user scheduled task."""
import csv
import io
import json
import mmap
import os
from pathlib import Path
import struct
import subprocess
import sys

FUSE_SENTINEL = b'dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX'
TASK_PREFIX = """
$taskName = 'Hoowire-' + [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$taskPath = '\\OpenHoo\\'
"""


def process_flags():
    return subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0


def powershell(script, payload=None):
    # Values travel as JSON over stdin, never as PowerShell source or shell text.
    system_shell = Path(os.environ.get('WINDIR', r'C:\Windows')) / 'System32/WindowsPowerShell/v1.0/powershell.exe'
    # PowerShell 7 terminals export a different PSModulePath. Passing that to
    # Windows PowerShell can shadow the system Security/ScheduledTasks modules.
    environment = {key: value for key, value in os.environ.items() if key.casefold() != 'psmodulepath'}
    environment['PSModulePath'] = str(system_shell.parent / 'Modules')
    prefix = """
$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$payload = [Console]::In.ReadToEnd() | ConvertFrom-Json
"""
    result = subprocess.run(
        [str(system_shell), '-NoProfile', '-NonInteractive', '-Command', prefix + script],
        input=json.dumps(payload or {}), encoding='utf-8', capture_output=True, timeout=60,
        creationflags=process_flags(), env=environment)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or 'Windows operation failed.')
    return result.stdout.strip()


def support_directory():
    if not os.environ.get('LOCALAPPDATA'):
        raise RuntimeError('LOCALAPPDATA is required for the Windows per-user runtime.')
    return Path(os.environ['LOCALAPPDATA']) / 'Hoowire'


def find_app():
    candidates = []
    local = os.environ.get('LOCALAPPDATA')
    if local:
        candidates += [Path(local) / 'Programs/Teamwire', Path(local) / 'Teamwire']
    for name in ('ProgramFiles', 'ProgramFiles(x86)'):
        if os.environ.get(name):
            candidates.append(Path(os.environ[name]) / 'Teamwire')
    found = [path for path in candidates if (path / 'Teamwire.exe').is_file()
             and (path / 'resources/app.asar').is_file()]
    if len(found) != 1:
        raise RuntimeError('Specify --app with the Teamwire installation directory (or Teamwire.exe).')
    return found[0]


def executable_fuses(executable):
    """Read Electron's documented V1 fuse wire; never change a fuse or PE byte."""
    with executable.open('rb') as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as data:
        if len(data) < 64 or data[:2] != b'MZ':
            raise RuntimeError('Teamwire.exe is not a Windows executable.')
        pe_offset = struct.unpack_from('<I', data, 0x3c)[0]
        if pe_offset + 24 > len(data) or data[pe_offset:pe_offset + 4] != b'PE\0\0':
            raise RuntimeError('Invalid Windows PE header.')
        start = data.find(FUSE_SENTINEL)
        if start < 0 or data.find(FUSE_SENTINEL, start + 1) >= 0:
            raise RuntimeError('Expected one Electron fuse wire in Teamwire.exe.')
        start += len(FUSE_SENTINEL)
        if start + 2 > len(data):
            raise RuntimeError('Truncated Electron fuse wire.')
        version, length = data[start], data[start + 1]
        states = data[start + 2:start + 2 + length]
        if version != 1 or length < 6 or len(states) != length or any(value not in b'01r' for value in states):
            raise RuntimeError('Unsupported Electron fuse format; a new Windows recipe is required.')
        # An enabled embedded hash binds app.asar to the signed PE. Fail closed
        # instead of invalidating Authenticode or disabling that protection.
        if states[4] != ord('0'):
            raise RuntimeError('Windows embedded ASAR integrity is enabled or unavailable; this build requires a new signing recipe.')
        return states.decode('ascii')


def verify_app(app):
    result = powershell("""
$signature = Get-AuthenticodeSignature -LiteralPath $payload.executable
if ($signature.Status -ne 'Valid' -or $null -eq $signature.SignerCertificate) {
    throw ('Teamwire.exe signature is not valid: ' + $signature.Status)
}
if ($signature.SignerCertificate.Subject -notmatch '(?:^|,\\s*)CN=Teamwire GmbH(?:,|$)') {
    throw 'The executable is not signed by the supported Teamwire publisher.'
}
$item = Get-Item -LiteralPath $payload.executable
@{version=$item.VersionInfo.ProductVersion; publisher=$signature.SignerCertificate.Subject} | ConvertTo-Json -Compress
""", {'executable': str((app / 'Teamwire.exe').resolve())})
    return json.loads(result)


def ensure_stopped():
    result = subprocess.run(['tasklist.exe', '/FI', 'IMAGENAME eq Teamwire.exe', '/FO', 'CSV', '/NH'],
                            capture_output=True, text=True, errors='replace', check=True, timeout=30,
                            creationflags=process_flags())
    for row in csv.reader(io.StringIO(result.stdout)):
        if row and row[0].casefold() == 'teamwire.exe':
            raise RuntimeError('Quit Teamwire before installing so its profile has only one writer.')


def loaded():
    return powershell(TASK_PREFIX + """
$task = Get-ScheduledTask -TaskPath $taskPath -ErrorAction SilentlyContinue |
    Where-Object { $_.TaskName -eq $taskName }
if ($task -and $task.State -ne 'Disabled') { 'true' } else { 'false' }
""") == 'true'


def disable_service():
    powershell(TASK_PREFIX + """
$task = Get-ScheduledTask -TaskPath $taskPath -ErrorAction SilentlyContinue |
    Where-Object { $_.TaskName -eq $taskName }
if ($task) {
    Stop-ScheduledTask -TaskName $taskName -TaskPath $taskPath -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $taskName -TaskPath $taskPath -Confirm:$false
}
""")


def enable_service(runtime, state_dir):
    python = Path(sys.executable)
    pythonw = python.with_name('pythonw.exe')
    if not pythonw.is_file():
        raise RuntimeError('A standard Python installation with pythonw.exe is required for the background task.')
    arguments = subprocess.list2cmdline([str(runtime / 'scripts/patcher.py'), 'check', '--quiet',
                                       '--state-dir', str(state_dir)])
    powershell(TASK_PREFIX + """
$scheduler = New-Object -ComObject 'Schedule.Service'
$scheduler.Connect()
try { $null = $scheduler.GetFolder('\\OpenHoo') }
catch { $null = $scheduler.GetFolder('\\').CreateFolder('OpenHoo') }
$user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$action = New-ScheduledTaskAction -Execute $payload.python -Argument $payload.arguments
$triggers = @(
    (New-ScheduledTaskTrigger -AtLogOn -User $user)
    (New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) -RepetitionInterval (New-TimeSpan -Minutes 2))
)
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew -StartWhenAvailable `
    -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Minutes 10)
Register-ScheduledTask -TaskName $taskName -TaskPath $taskPath -Action $action `
    -Trigger $triggers -Settings $settings -Principal $principal -Force | Out-Null
""", {'python': str(pythonw), 'arguments': arguments})
    return {'enabled': loaded(), 'agent': 'Task Scheduler: OpenHoo / Hoowire (current user)'}
