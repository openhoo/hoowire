#!/usr/bin/env python3
"""Synthetic isolated logical-restore drill; never connects to existing databases."""
import base64
import hashlib
import http.client
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import subprocess
import tempfile
import sys
import time
from datetime import datetime, timezone
from uuid import UUID, uuid4

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.padding import PKCS7

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(
    os.environ.get('HOOWIRE_RECOVERY_BINARY', str(ROOT / 'target/debug/hoowire'))
).resolve()
TEST_IMAGE = os.environ.get('HOOWIRE_TEST_POSTGRES_IMAGE', 'postgres:17-alpine')
MIGRATION_COUNT = len(list((ROOT / 'crates/hoowire-db/migrations').glob('*.sql')))
sys.path.insert(0, str(ROOT / 'tests'))
from source_manifest import source_manifest

MASTER = secrets.token_bytes(32)
CONFIG = {}
CONTAINERS = []


def run(*args, **kwargs):
    try:
        return subprocess.run(args, check=True, capture_output=True, **kwargs)
    except subprocess.CalledProcessError as error:
        print(error.stderr.decode(errors='replace'), flush=True)
        raise


def query(container, sql):
    return (
        run(
            'docker', 'exec', container,
            'psql', '-X', '-U', 'hoowire', '-d', 'hoowire', '-At', '-c', sql,
        )
        .stdout.decode()
        .strip()
    )


def counts(container):
    tables = query(
        container,
        "SELECT tablename FROM pg_tables "
        "WHERE schemaname='public' ORDER BY tablename",
    ).splitlines()
    assert tables and all(name.isidentifier() for name in tables)
    terms = ','.join(f"'{name}',(SELECT count(*) FROM {name})" for name in tables)
    return json.loads(query(container, 'SELECT json_build_object(' + terms + ')'))


def decrypt(raw, encoded=True, key=None):
    envelope = base64.b64decode(raw, validate=True) if encoded else raw
    iv = envelope[:16]
    assert hashlib.sha256(iv).digest() == envelope[16:48]
    decryptor = Cipher(
        algorithms.AES(key or CONFIG['secret'].encode()), modes.CBC(iv)
    ).decryptor()
    padded = decryptor.update(envelope[48:]) + decryptor.finalize()
    unpadder = PKCS7(128).unpadder()
    return unpadder.update(padded) + unpadder.finalize()


def encrypt(raw, key, encoded=True):
    iv = os.urandom(16)
    padder = PKCS7(128).padder()
    padded = padder.update(raw) + padder.finalize()
    encryptor = Cipher(algorithms.AES(key), modes.CBC(iv)).encryptor()
    envelope = (
        iv + hashlib.sha256(iv).digest()
        + encryptor.update(padded) + encryptor.finalize()
    )
    return base64.b64encode(envelope) if encoded else envelope


def request(port, path, method='GET', body=None, headers=None, expected=200):
    connection = http.client.HTTPConnection('127.0.0.1', port, timeout=5)
    connection.request(method, path, body=body, headers=headers or {})
    response = connection.getresponse()
    content = response.read()
    status = response.status
    connection.close()
    assert status == expected, (path, status)
    return content


def call(port, path, method='GET', value=None):
    key = CONFIG['secret'].encode()
    headers = {'From': CONFIG['appid']}
    body = None
    if value is not None:
        body = encrypt(json.dumps(value).encode(), key)
        headers['Content-Type'] = 'application/octet-stream'
    return json.loads(decrypt(request(port, '/v20' + path, method, body, headers)))


def multipart(port, path, content, content_type):
    boundary = 'hoowire-restore-' + secrets.token_hex(16)
    body = (
        (
            f'--{boundary}\r\n'
            'Content-Disposition: form-data; name="f"; filename="f"\r\n'
            f'Content-Type: {content_type}\r\n\r\n'
        ).encode()
        + content
        + f'\r\n--{boundary}--\r\n'.encode()
    )
    return request(
        port, path, 'POST', body,
        {
            'From': CONFIG['appid'],
            'Content-Type': 'multipart/form-data; boundary=' + boundary,
        },
    )


def stop(server):
    server.terminate()
    try:
        server.wait(timeout=8)
    except subprocess.TimeoutExpired:
        server.kill()
        server.wait()


def start(environment):
    with socket.socket() as reserve:
        reserve.bind(('127.0.0.1', 0))
        port = reserve.getsockname()[1]
    environment = dict(environment, HOOWIRE_BIND=f'127.0.0.1:{port}')
    server = subprocess.Popen(
        [str(BINARY), 'serve'],
        env=environment,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        for attempt in range(100):
            try:
                request(port, '/health/ready')
                return server, port
            except (OSError, AssertionError):
                if server.poll() is not None:
                    raise RuntimeError('HooWire failed startup')
                time.sleep(0.1)
        raise RuntimeError('server did not become ready')
    except BaseException:
        stop(server)
        raise


def postgres():
    password = secrets.token_hex(24)
    container = (
        run(
            'docker', 'run', '--detach', '--rm',
            '--label', 'dev.openhoo.project=hoowire-recovery-drill',
            '--publish', '127.0.0.1::5432',
            '--env', 'POSTGRES_USER=hoowire',
            '--env', 'POSTGRES_DB=hoowire',
            '--env', 'POSTGRES_PASSWORD=' + password,
            '--env', 'POSTGRES_INITDB_ARGS=--auth-host=scram-sha-256',
            TEST_IMAGE,
        )
        .stdout.decode()
        .strip()
    )
    CONTAINERS.append(container)
    for attempt in range(100):
        if subprocess.run(
            [
                'docker', 'exec', '--env', 'PGPASSWORD=' + password, container,
                'psql', '-X', '-w', '-h', '127.0.0.1', '-U', 'hoowire', '-d', 'hoowire',
                '-v', 'ON_ERROR_STOP=1', '-Atqc', 'SELECT 1',
            ],
            capture_output=True,
        ).returncode == 0:
            break
        time.sleep(0.1)
    else:
        raise RuntimeError('fresh PostgreSQL did not become ready')
    db_port = int(
        run('docker', 'port', container, '5432/tcp')
        .stdout.decode()
        .strip()
        .rsplit(':', 1)[1]
    )
    return container, f'postgres://hoowire:{password}@127.0.0.1:{db_port}/hoowire'


def terminate(_signum, _frame):
    raise SystemExit(143)


if not BINARY.is_file():
    raise SystemExit('Build the test executable first: cargo build --locked --bin hoowire')

temporary = Path(tempfile.mkdtemp(prefix='hoowire-recovery-'))
server = None
source_server = None
signal.signal(signal.SIGTERM, terminate)
try:
    source, source_url = postgres()
    original_assets = temporary / 'original-assets'
    source_env = dict(
        os.environ,
        DATABASE_URL=source_url,
        HOOWIRE_MASTER_KEY=base64.b64encode(MASTER).decode(),
        HOOWIRE_DATA_DIR=str(original_assets),
        HOOWIRE_USER_PASSWORD=secrets.token_urlsafe(24),
        HOOWIRE_SDK_SECRET=base64.b64encode(secrets.token_bytes(24)).decode(),
    )
    binary = str(BINARY)
    run(binary, 'migrate', env=source_env)
    tenant = str(
        UUID(
            run(binary, 'create-tenant', '--name', 'Recovery fixture', env=source_env)
            .stdout.decode()
            .strip()
            .splitlines()[-1]
        )
    )

    def account(name, bot=False):
        email = f'recovery-{uuid4()}@example.invalid'
        args = [
            binary, 'create-user', '--tenant', tenant,
            '--email', email, '--first-name', name,
        ]
        if bot:
            args.append('--bot')
        user = str(
            UUID(run(*args, env=source_env).stdout.decode().strip().splitlines()[-1])
        )
        return user, email

    bot, _ = account('Restore Bot', bot=True)
    human, email = account('Restore Human')
    credentials_file = temporary / 'sdk-credentials.json'
    run(
        binary, 'create-integration', '--user', bot,
        '--credentials-out', str(credentials_file), env=source_env,
    )
    credentials = json.loads(credentials_file.read_text())
    CONFIG = {'appid': credentials['app_id'], 'secret': credentials['secret']}
    source_server, source_port = start(source_env)
    chat = call(
        source_port, '/t/?legacy_circle_ids=true', 'PUT',
        {
            'chatType': 'group',
            'col': CONFIG['secret'],
            'title': 'Recovery proof',
            'users': [bot, human],
            'groups': [],
            'roles': [],
        },
    )['tid']
    png = (ROOT / 'tests/fixtures/pixel.png').read_bytes()
    multipart(source_port, f'/v20/u/profile/{bot}/image/upload', png, 'image/png')
    original_bytes = encrypt(
        b'Independent immutable recovery attachment \x00\xff',
        CONFIG['secret'].encode(),
        encoded=False,
    )
    asset_response = json.loads(
        multipart(
            source_port, f'/v20/upload?tid={chat}',
            original_bytes, 'application/octet-stream',
        )
    )
    asset_id = str(UUID(asset_response['id']))
    message = call(
        source_port, '/t', 'POST',
        {
            'tid': chat,
            'cid': str(uuid4()),
            'msg': encrypt(
                'Wiederherstellung geprüft 🌍'.encode(), CONFIG['secret'].encode()
            ).decode(),
            'isAlert': False,
        },
    )
    private = ec.generate_private_key(ec.SECP256K1())
    public = private.public_key().public_bytes(
        serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint
    ).hex()
    handshake = json.loads(
        request(
            source_port, '/v20/u/sessions/', 'POST',
            json.dumps({'client_pubkey': public}),
            {'Content-Type': 'application/json'},
        )
    )
    peer = ec.EllipticCurvePublicKey.from_encoded_point(
        ec.SECP256K1(), bytes.fromhex(handshake['server_pubkey'])
    )
    transport = base64.b64encode(private.exchange(ec.ECDH(), peer)[:24])
    human_credentials = {
        'email_hash': hashlib.sha256(email.encode()).hexdigest(),
        'password': source_env['HOOWIRE_USER_PASSWORD'],
    }
    identity = json.loads(
        decrypt(
            request(
                source_port, '/v20/u/login/' + handshake['session_id'] + '/', 'POST',
                encrypt(json.dumps(human_credentials).encode(), transport),
                {'Content-Type': 'application/octet-stream'},
            ),
            key=transport,
        )
    )
    assert identity['user_id'] == human
    human_token = identity['from_id']
    assert json.loads(
        decrypt(
            request(source_port, '/v20/u/name', headers={'From': human_token}),
            key=transport,
        )
    )['user_id'] == human
    stop(source_server)
    source_server = None

    original_counts = counts(source)
    assert original_counts['_sqlx_migrations'] == MIGRATION_COUNT
    backup = temporary / 'database.dump'
    with backup.open('wb') as output:
        subprocess.run(
            [
                'docker', 'exec', source,
                'pg_dump', '-U', 'hoowire', '-d', 'hoowire', '--format=custom',
            ],
            stdout=output,
            check=True,
        )
    assets = temporary / 'restored-assets'
    shutil.copytree(original_assets, assets)
    restored, restored_url = postgres()
    with backup.open('rb') as input_file:
        result = subprocess.run(
            [
                'docker', 'exec', '-i', restored,
                'pg_restore', '-U', 'hoowire', '-d', 'hoowire',
                '--no-owner', '--no-privileges', '--exit-on-error',
            ],
            stdin=input_file,
            capture_output=True,
        )
        if result.returncode:
            print(result.stderr.decode(), flush=True)
            result.check_returncode()
    restored_counts = counts(restored)
    assert restored_counts == original_counts
    stored_assets = json.loads(
        query(
            restored,
            "SELECT json_agg(json_build_object('id',id,'digest',digest,'size',size)) "
            "FROM assets",
        )
    )
    for asset in stored_assets:
        asset_content = (assets / asset['id']).read_bytes()
        assert len(asset_content) == asset['size']
        assert hashlib.sha256(asset_content).hexdigest() == asset['digest']
    environment = dict(
        source_env, DATABASE_URL=restored_url, HOOWIRE_DATA_DIR=str(assets)
    )
    server, http_port = start(environment)
    assert call(http_port, '/u/name')['user_id'] == bot
    subscriptions = call(http_port, '/u/subscriptions')
    assert len(subscriptions) == 1 and subscriptions[0]['col'] == CONFIG['secret']
    assert request(
        http_port, '/v20/serve/' + asset_id, headers={'From': CONFIG['appid']}
    ) == original_bytes
    avatar = json.loads(
        query(
            restored,
            "SELECT row_to_json(a) FROM ("
            "SELECT tenant_id,user_id,encode(bytes_sealed,'hex') AS sealed "
            f"FROM avatars WHERE user_id='{bot}') a",
        )
    )
    encrypted_avatar = bytes.fromhex(avatar['sealed'])
    aad = f"hoowire-avatar:v1:{avatar['tenant_id']}:user:{avatar['user_id']}".encode()
    expected_avatar = AESGCM(MASTER).decrypt(
        encrypted_avatar[4:16], encrypted_avatar[16:], aad
    )
    actual_avatar = decrypt(
        request(
            http_port, '/v20/u/profile/' + bot + '/image',
            headers={'From': CONFIG['appid']},
        ),
        encoded=False,
    )
    assert actual_avatar == expected_avatar == png
    restored_human = json.loads(
        decrypt(
            request(http_port, '/v20/u/name', headers={'From': human_token}),
            key=transport,
        )
    )
    assert restored_human['user_id'] == human
    request(http_port, '/v20/u/logout', 'POST', headers={'From': human_token})
    request(http_port, '/v20/u/name', headers={'From': human_token}, expected=403)
    feed = call(http_port, '/feed')
    assert any(row['mid'] == message['mid'] for row in feed['messages'])
    report = {
        'generated_at': datetime.now(timezone.utc).isoformat(),
        'passed': True,
        'source_manifest': source_manifest(ROOT),
        'binary_sha256': hashlib.sha256(BINARY.read_bytes()).hexdigest(),
        'drill_script_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        'postgres_image': TEST_IMAGE,
        'strategy': (
            'quiesced PostgreSQL custom-format logical dump plus immutable asset '
            'copy between two fresh isolated PostgreSQL containers'
        ),
        'database_version': query(restored, 'SHOW server_version'),
        'table_counts_before_postrestore_mutations': restored_counts,
        'backup_bytes': backup.stat().st_size,
        'copied_and_digest_verified_assets': len(stored_assets),
        'human_session_restore_policy': (
            'preserve synthetic prebackup session to verify key recovery, then '
            'explicitly log it out; production policy is operator-selected'
        ),
        'checks': [
            'every public table count preserved',
            f'{MIGRATION_COUNT} migration checksums accepted by executable',
            'integration authentication with original sealed secret',
            'conversation key opens with original master key',
            'every fixture asset copied and SHA-256 verified',
            'authorized attachment HTTP download exact byte match',
            'avatar AEAD with original master key and session-encrypted HTTP byte match',
            (
                'prebackup human session authenticates and decrypts after full DB '
                'restore into new process'
            ),
            'restored human session logout persists and rejects subsequent access',
            'durable feed contains prebackup message after recovery',
        ],
        'limits': [
            'synthetic local data only',
            'operator privileges on recovery PostgreSQL; role separation tested separately',
            (
                'no production backup controller, point-in-time recovery or '
                'destructive host-loss drill'
            ),
        ],
    }
    (ROOT / 'artifacts').mkdir(exist_ok=True)
    (ROOT / 'artifacts/recovery-drill.json').write_text(
        json.dumps(report, indent=2) + '\n'
    )
    print(json.dumps({key: value for key, value in report.items() if key != 'source_manifest'}))
finally:
    if source_server is not None:
        stop(source_server)
    if server is not None:
        stop(server)
    for container in reversed(CONTAINERS):
        subprocess.run(
            ['docker', 'rm', '--force', '--volumes', container], capture_output=True
        )
    shutil.rmtree(temporary)
