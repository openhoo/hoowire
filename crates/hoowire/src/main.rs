use base64::{Engine, engine::general_purpose::STANDARD};
use clap::{Parser, Subcommand};
use hoowire_api::CorsConfig;
use hoowire_blob::BlobStore;
use hoowire_db::Repository;
use hoowire_service::{App, DEFAULT_TENANT, random_secret};
use std::{net::SocketAddr, path::PathBuf};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(
    version,
    about,
    long_about = "HooWire — independent Rust server targeting Teamwire API20. See docs/compatibility.md for verified scope."
)]
struct Cli {
    #[arg(long, env = "DATABASE_URL", global = true, hide_env_values = true)]
    database_url: Option<String>,
    #[arg(
        long,
        env = "HOOWIRE_BIND",
        default_value = "127.0.0.1:8080",
        global = true
    )]
    bind: SocketAddr,
    #[arg(
        long,
        env = "HOOWIRE_DATA_DIR",
        default_value = "data/assets",
        global = true
    )]
    data_dir: PathBuf,
    #[arg(
        long,
        env = "HOOWIRE_DB_MAX_CONNECTIONS",
        default_value_t = 10,
        global = true
    )]
    db_max_connections: u32,
    /// Exact web UI origins, comma separated; no paths or trailing slashes.
    #[arg(
        long,
        env = "HOOWIRE_CORS_WEB_ORIGINS",
        default_value = "",
        global = true
    )]
    cors_web_origins: String,
    /// Allow literal opaque Origin:null alongside the exact web-origin allowlist.
    #[arg(
        long,
        env = "HOOWIRE_CORS_ALLOW_DESKTOP",
        default_value_t = true,
        action = clap::ArgAction::Set,
        global = true
    )]
    cors_allow_desktop: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start HTTP/WSS upstream for a TLS reverse proxy.
    Serve,
    /// Apply versioned database migrations using the migration owner role.
    Migrate {
        /// Per-statement migration time budget; serving keeps its 15-second limit.
        #[arg(
            long,
            env = "HOOWIRE_MIGRATION_STATEMENT_TIMEOUT_SECS",
            default_value_t = 300,
            value_parser = clap::value_parser!(u32).range(1..=3600)
        )]
        statement_timeout_secs: u32,
    },
    /// Generate a new encryption master key. Store it as HOOWIRE_MASTER_KEY.
    Keygen,
    /// Create an organization. Prints its UUID for subsequent account provisioning.
    CreateTenant {
        #[arg(long)]
        name: String,
    },
    /// Provision or replace a directory circle/role and its normalized members.
    CreateDirectory {
        #[arg(long, default_value_t = DEFAULT_TENANT)]
        tenant: Uuid,
        #[arg(long, value_parser = ["group", "role"])]
        kind: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        id: Option<Uuid>,
        #[arg(long, value_delimiter = ',')]
        members: Vec<Uuid>,
    },
    /// Provision an account locally. Reads password from HOOWIRE_USER_PASSWORD.
    CreateUser {
        #[arg(long)]
        email: String,
        #[arg(long)]
        first_name: String,
        #[arg(long, default_value = "")]
        last_name: String,
        #[arg(long)]
        bot: bool,
        #[arg(long, default_value_t = DEFAULT_TENANT)]
        tenant: Uuid,
    },
    /// Provision an SDK integration; credentials are written to a new private file.
    CreateIntegration {
        #[arg(long)]
        user: Uuid,
        #[arg(long)]
        app_id: Option<String>,
        #[arg(long)]
        credentials_out: PathBuf,
    },
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hoowire=info".into()),
        )
        .init();
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            // Never print database URLs, SQL errors, credentials or transport payloads.
            tracing::error!(error = %error, "command failed");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Serve);
    if matches!(command, Command::Keygen) {
        println!("{}", random_secret(32));
        return Ok(());
    }
    let cors = if matches!(command, Command::Serve) {
        CorsConfig::new(
            cli.cors_allow_desktop,
            cli.cors_web_origins
                .split(',')
                .map(str::trim)
                .filter(|origin| !origin.is_empty()),
        )?
    } else {
        CorsConfig::default()
    };
    if !(2..=100).contains(&cli.db_max_connections) {
        return Err("HOOWIRE_DB_MAX_CONNECTIONS must be in 2..=100 (one connection is reserved for notifications)".into());
    }
    let url = Zeroizing::new(cli.database_url.ok_or("DATABASE_URL is required")?);
    if let Command::Migrate {
        statement_timeout_secs,
    } = command
    {
        let repository = Repository::connect_for_migrations(
            &url,
            cli.db_max_connections,
            statement_timeout_secs,
        )
        .await?;
        repository.migrate().await?;
        repository.close().await;
        println!("PostgreSQL migrations applied");
        return Ok(());
    }
    let repository = Repository::connect(&url, cli.db_max_connections).await?;
    repository.verify_schema().await?;
    match &command {
        Command::CreateTenant { name } => {
            println!("{}", repository.create_tenant(name).await?);
            repository.close().await;
            return Ok(());
        }
        Command::CreateDirectory {
            tenant,
            kind,
            title,
            id,
            members,
        } => {
            let id = id.unwrap_or_else(Uuid::new_v4);
            repository
                .upsert_directory_entry(
                    *tenant,
                    id,
                    kind,
                    serde_json::json!({"title":title,"user_ids":members}),
                )
                .await?;
            println!("{id}");
            repository.close().await;
            return Ok(());
        }
        _ => {}
    }
    let encoded = Zeroizing::new(
        std::env::var("HOOWIRE_MASTER_KEY")
            .map_err(|_| "HOOWIRE_MASTER_KEY is required (generate with hoowire keygen)")?,
    );
    let decoded = Zeroizing::new(
        STANDARD
            .decode(encoded.as_bytes())
            .map_err(|_| "HOOWIRE_MASTER_KEY must be base64")?,
    );
    let master: Zeroizing<[u8; 32]> = Zeroizing::new(
        decoded
            .as_slice()
            .try_into()
            .map_err(|_| "HOOWIRE_MASTER_KEY must decode to 32 bytes")?,
    );
    let blobs = BlobStore::open(cli.data_dir).await?;
    let app = App::new(repository.clone(), blobs, &master);
    match command {
        Command::CreateUser {
            email,
            first_name,
            last_name,
            bot,
            tenant,
        } => {
            let password = Zeroizing::new(
                std::env::var("HOOWIRE_USER_PASSWORD")
                    .map_err(|_| "HOOWIRE_USER_PASSWORD is required")?,
            );
            let user = app
                .provision_user(tenant, &email, password, &first_name, &last_name, bot)
                .await?;
            println!("{}", user.id);
        }
        Command::CreateIntegration {
            user,
            app_id,
            credentials_out,
        } => {
            let user = repository.user(user).await?;
            let secret = Zeroizing::new(
                std::env::var("HOOWIRE_SDK_SECRET").unwrap_or_else(|_| random_secret(24)),
            );
            let app_id = app_id.unwrap_or_else(|| Uuid::new_v4().to_string());
            let data = Zeroizing::new(serde_json::to_vec_pretty(
                &serde_json::json!({"app_id":app_id,"secret":*secret}),
            )?);
            // Reserve file first: failure must never overwrite existing credentials.
            let mut options = std::fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&credentials_out)
                .map_err(|_| "cannot create credentials output file")?;
            if let Err(error) = app.provision_integration(&user, &app_id, &secret).await {
                drop(file);
                let _ = std::fs::remove_file(&credentials_out);
                return Err(error.into());
            }
            use std::io::Write;
            file.write_all(&data)?;
            file.sync_all()?;
            println!(
                "Integration created; credentials saved to {}",
                credentials_out.display()
            );
        }
        Command::Serve => {
            app.health().await?;
            let listener = tokio::net::TcpListener::bind(cli.bind).await?;
            tracing::info!(address = %listener.local_addr()?, "HooWire ready");
            axum_serve(listener, app.clone(), cors).await?;
        }
        _ => unreachable!(),
    }
    app.close().await;
    Ok(())
}

async fn axum_serve(
    listener: tokio::net::TcpListener,
    app: App,
    cors: CorsConfig,
) -> Result<(), std::io::Error> {
    use std::future::IntoFuture;
    // Axum lives in the transport adapter. No SQL or protocol logic in the executable.
    hoowire_api::serve_with_cors(listener, app, cors)
        .into_future()
        .await
}
