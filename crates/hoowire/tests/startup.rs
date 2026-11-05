//! Invalid executable configuration must fail before an HTTP listener is exposed.
#![cfg(unix)]

mod common;

use base64::{Engine, engine::general_purpose::STANDARD};
use common::{ServerProcess, capture};
use std::{
    process::{Command, Output, Stdio},
    time::Duration,
};
use tokio::{net::TcpStream, time::timeout};
use uuid::Uuid;

fn bare_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hoowire"));
    command
        .env_remove("DATABASE_URL")
        .env_remove("HOOWIRE_MASTER_KEY")
        .env("HOOWIRE_BIND", "127.0.0.1:0")
        .env("HOOWIRE_DB_MAX_CONNECTIONS", "4")
        .env("HOOWIRE_CORS_WEB_ORIGINS", "")
        .env("HOOWIRE_CORS_ALLOW_DESKTOP", "true")
        .env("TOKIO_WORKER_THREADS", "2")
        .env("RUST_LOG", "hoowire=info")
        .stdin(Stdio::null());
    command
}

fn assert_failure(output: Output, code: i32, message: &str) {
    assert_eq!(output.status.code(), Some(code));
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.contains(message),
        "expected CLI diagnostic {message:?}, got {output:?}"
    );
}

#[tokio::test]
async fn serving_without_a_database_url_fails_with_a_clear_cli_diagnostic() {
    assert_failure(
        capture(bare_command().arg("serve")).await,
        1,
        "DATABASE_URL is required",
    );
}

#[tokio::test]
async fn executable_rejects_pool_and_origin_configuration_before_connecting() {
    for value in ["0", "1", "101"] {
        let mut command = bare_command();
        command
            .arg("serve")
            .env("HOOWIRE_DB_MAX_CONNECTIONS", value);
        assert_failure(
            capture(&mut command).await,
            1,
            "HOOWIRE_DB_MAX_CONNECTIONS must be in 2..=100",
        );
    }
    let mut command = bare_command();
    command
        .arg("serve")
        .env("HOOWIRE_CORS_WEB_ORIGINS", "https://example.invalid/path");
    assert_failure(
        capture(&mut command).await,
        1,
        "CORS web origins must be exact serialized http(s) origins",
    );
}

#[tokio::test]
async fn executable_bounds_migration_timeouts_and_rejects_invalid_bind_addresses() {
    for value in ["0", "3601"] {
        assert_failure(
            capture(bare_command().args(["migrate", "--statement-timeout-secs", value])).await,
            2,
            "invalid value",
        );
    }
    assert_failure(
        capture(bare_command().args(["--bind", "invalid-address", "serve"])).await,
        2,
        "invalid value",
    );
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn invalid_master_keys_never_start_a_server() {
    let server = ServerProcess::new();
    assert_eq!(server.cli(&["migrate"], &[]).await.status.code(), Some(0));
    let mut command = server.command();
    command.arg("serve").env_remove("HOOWIRE_MASTER_KEY");
    assert_failure(
        capture(&mut command).await,
        1,
        "HOOWIRE_MASTER_KEY is required",
    );
    for (value, message) in [
        ("!".to_owned(), "HOOWIRE_MASTER_KEY must be base64"),
        (
            STANDARD.encode([0x42; 31]),
            "HOOWIRE_MASTER_KEY must decode to 32 bytes",
        ),
        (
            STANDARD.encode([0x42; 33]),
            "HOOWIRE_MASTER_KEY must decode to 32 bytes",
        ),
    ] {
        assert_failure(
            server
                .cli(&["serve"], &[("HOOWIRE_MASTER_KEY", &value)])
                .await,
            1,
            message,
        );
    }
    assert!(
        timeout(
            Duration::from_millis(500),
            TcpStream::connect(server.address)
        )
        .await
        .unwrap()
        .is_err(),
        "invalid master keys must never expose a listener"
    );
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn serving_refuses_an_unmigrated_schema_and_explicit_migration_allows_startup() {
    let mut server = ServerProcess::new();
    // An absent private search path hides public tables without mutating any
    // schema, so this remains isolated from other integration tests in this DB.
    let absent_schema = format!("hoowire_unmigrated_{}", Uuid::new_v4().simple());
    let separator = if server.database_url.contains('?') {
        '&'
    } else {
        '?'
    };
    let unmigrated_url = format!(
        "{}{separator}options=-csearch_path%3D{absent_schema}",
        server.database_url
    );
    assert_failure(
        server
            .cli(&["serve"], &[("DATABASE_URL", &unmigrated_url)])
            .await,
        1,
        "service temporarily unavailable",
    );
    assert!(
        timeout(
            Duration::from_millis(500),
            TcpStream::connect(server.address)
        )
        .await
        .unwrap()
        .is_err(),
        "unmigrated serving must never expose a listener"
    );
    assert_eq!(server.cli(&["migrate"], &[]).await.status.code(), Some(0));
    server.start().await;
    assert!(
        server
            .http
            .get(server.url("/health/ready"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    server.stop().await;
}
