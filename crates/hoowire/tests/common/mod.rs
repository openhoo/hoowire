//! Owned process fixtures shared by executable integration tests.
#![allow(dead_code)]

use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::Client;
use std::{
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    process::{Child, Command, ExitStatus, Output, Stdio},
    time::Duration,
};
use tokio::time::{Instant, sleep};
use uuid::Uuid;

pub const MASTER_KEY: [u8; 32] = [0x42; 32];
pub const SECRET: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

pub async fn capture(command: &mut Command) -> Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut process = OwnedChild::spawn(command);
    process
        .wait_until(Instant::now() + Duration::from_secs(10))
        .await;
    process.output()
}

pub struct OwnedChild(Option<Child>);

impl OwnedChild {
    pub fn spawn(command: &mut Command) -> Self {
        Self(Some(command.spawn().expect("start hoowire executable")))
    }

    pub fn child(&mut self) -> &mut Child {
        self.0.as_mut().expect("child process is owned")
    }

    pub async fn wait_until(&mut self, deadline: Instant) -> ExitStatus {
        loop {
            if let Some(status) = self.child().try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "child process exit timeout");
            sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn output(mut self) -> Output {
        self.0
            .take()
            .unwrap()
            .wait_with_output()
            .expect("collect bounded CLI output")
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

/// Own this test's server and asset path, including on assertion failure.
pub struct ServerProcess {
    process: Option<OwnedChild>,
    pub data: PathBuf,
    pub database_url: String,
    pub address: SocketAddr,
    pub http: Client,
}

impl ServerProcess {
    pub fn new() -> Self {
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        Self {
            process: None,
            data: std::env::temp_dir().join(format!("hoowire-process-{}", Uuid::new_v4())),
            database_url: std::env::var("HOOWIRE_TEST_DATABASE_URL")
                .expect("isolated PostgreSQL URL required"),
            address,
            http: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap(),
        }
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hoowire"));
        command
            .env("DATABASE_URL", &self.database_url)
            .env("HOOWIRE_MASTER_KEY", STANDARD.encode(MASTER_KEY))
            .env("HOOWIRE_BIND", self.address.to_string())
            .env("HOOWIRE_DATA_DIR", &self.data)
            .env("HOOWIRE_DB_MAX_CONNECTIONS", "4")
            .env("HOOWIRE_CORS_WEB_ORIGINS", "")
            .env("HOOWIRE_CORS_ALLOW_DESKTOP", "true")
            .env("TOKIO_WORKER_THREADS", "2")
            .env("RUST_LOG", "hoowire=info")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    pub fn child(&mut self) -> &mut Child {
        self.process
            .as_mut()
            .expect("server process was spawned")
            .child()
    }

    pub async fn start(&mut self) {
        if let Some(process) = &mut self.process {
            assert!(
                process.child().try_wait().unwrap().is_some(),
                "previous server must exit before restarting"
            );
        }
        self.process = Some(OwnedChild::spawn(self.command().arg("serve")));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                self.child().try_wait().unwrap().is_none(),
                "server exited before readiness"
            );
            if let Ok(response) = self.http.get(self.url("/health/ready")).send().await
                && response.status().is_success()
            {
                return;
            }
            assert!(Instant::now() < deadline, "server readiness timeout");
            sleep(Duration::from_millis(25)).await;
        }
    }

    pub fn terminate(&mut self) {
        let signal = Command::new("kill")
            .arg("-TERM")
            .arg(self.child().id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("send SIGTERM to owned server PID");
        assert!(signal.success(), "SIGTERM command failed");
    }

    pub async fn wait_until(&mut self, deadline: Instant) -> ExitStatus {
        self.process
            .as_mut()
            .expect("server process was spawned")
            .wait_until(deadline)
            .await
    }

    pub async fn stop(&mut self) {
        self.terminate();
        let status = self
            .wait_until(Instant::now() + Duration::from_secs(5))
            .await;
        assert_eq!(status.code(), Some(0), "server must exit successfully");
    }

    pub async fn cli(&self, args: &[&str], changes: &[(&str, &str)]) -> Output {
        let mut command = self.command();
        command.args(args);
        for (key, value) in changes {
            command.env(key, value);
        }
        capture(&mut command).await
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        // Stop the child before removing files it may still use.
        drop(self.process.take());
        let _ = std::fs::remove_dir_all(&self.data);
    }
}
