#![cfg_attr(windows, windows_subsystem = "windows")]
use agent_observatory::{
    config::{self, AuthMode, Config, WebTransport},
    desktop,
    state::AppState,
};
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
#[derive(Parser)]
#[command(version, about = "Observe and manage independent ACP agents")]
struct Args {
    #[arg(long, env = "OBSERVATORY_HOME")]
    home: Option<PathBuf>,
    #[arg(long)]
    no_browser: bool,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    Init {
        #[arg(long)]
        owner: String,
        #[arg(long, default_value = "0.0.0.0")]
        bind: String,
        #[arg(long, default_value = "0")]
        port: u16,
        #[arg(long, default_value = "")]
        public_url: String,
        #[arg(long, default_value = "agent-observatory")]
        pam_service: String,
        #[arg(long, value_parser=["pam","local-password"], default_value="pam")]
        auth_mode: String,
        #[arg(long)]
        password_file: Option<PathBuf>,
        #[arg(long)]
        local_only: bool,
    },
    Serve,
    #[command(hide = true)]
    ApplyUpdate {
        request_id: String,
    },
    #[command(hide = true)]
    ServeUpdate {
        request_id: String,
        nonce: String,
    },
    Status,
    Config,
    Api {
        method: String,
        path: String,
        #[arg(long)]
        body_file: Option<PathBuf>,
    },
    Credential {
        name: String,
        #[arg(long)]
        value_file: PathBuf,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args = Args::parse();
    let home = args.home.map(Ok).unwrap_or_else(desktop::default_home)?;
    let Some(command) = args.command else {
        let result = desktop::launch(home, args.no_browser).await;
        if !args.no_browser
            && let Err(error) = &result
        {
            amplifier_native_platform::show_launch_error(
                &amplifier_native_platform::OBSERVATORY,
                &format!(
                    "Observatory could not open: {error:#}\n\nYour saved installations have been preserved."
                ),
            );
        }
        return result;
    };
    match command {
        Command::Init {
            owner,
            bind,
            port,
            public_url,
            pam_service,
            auth_mode,
            password_file,
            local_only,
        } => {
            config::private_dir(&home)?;
            ensure!(
                !home.join("config.json").exists(),
                "An installation already exists here"
            );
            let auth_mode = if auth_mode == "local-password" {
                AuthMode::LocalPassword
            } else {
                AuthMode::Pam
            };
            ensure!(
                !local_only || auth_mode == AuthMode::LocalPassword,
                "Local-only setup requires an Observatory password"
            );
            if auth_mode == AuthMode::LocalPassword {
                let file = password_file
                    .context("Supply --password-file for noninteractive local account setup")?;
                let password = std::fs::read_to_string(file)?;
                agent_observatory::auth::initialize_local_password(
                    &home,
                    password.trim_end_matches(['\r', '\n']),
                )?;
            } else {
                ensure!(
                    password_file.is_none(),
                    "Password files apply only to local-password setup"
                );
            }
            let (tls_cert, tls_key, tls_ca) = generate_tls(&home)?;
            let bind = if local_only {
                "127.0.0.1".to_owned()
            } else {
                bind
            };
            let port = if port == 0 {
                std::net::TcpListener::bind((bind.as_str(), 0))?
                    .local_addr()?
                    .port()
            } else {
                port
            };
            let c = Config {
                id: uuid::Uuid::new_v4().to_string(),
                owner,
                bind,
                port,
                public_url: if local_only {
                    format!("http://127.0.0.1:{port}")
                } else {
                    public_url
                },
                auth_mode,
                web_transport: if local_only {
                    WebTransport::LocalHttp
                } else {
                    WebTransport::Https
                },
                pam_service,
                tls_cert,
                tls_key,
                tls_ca,
                embed_origins: vec![],
            };
            c.save(&home)?;
            AppState::open(home.clone())?;
            println!(
                "{}",
                json!({"home":home,"id":c.id,"port":c.port,"url":if c.public_url.is_empty(){format!("https://localhost:{}",c.port)}else{c.public_url}})
            );
        }
        Command::Serve => desktop::serve(home.clone(), false).await?,
        Command::ServeUpdate { request_id, nonce } => {
            desktop::serve_guarded(home.clone(), false, Some((request_id, nonce))).await?
        }
        Command::ApplyUpdate { request_id } => {
            agent_observatory::desktop_update::apply(&home, &request_id).await?
        }
        Command::Config => println!("{}", serde_json::to_string_pretty(&Config::load(&home)?)?),
        Command::Status => println!(
            "{}",
            serde_json::to_string_pretty(&api(&home, "GET", "status", None).await?)?
        ),
        Command::Api {
            method,
            path,
            body_file,
        } => {
            let body = body_file
                .map(std::fs::read)
                .transpose()?
                .map(|b| serde_json::from_slice::<Value>(&b))
                .transpose()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&api(&home, &method, &path, body).await?)?
            );
        }
        Command::Credential { name, value_file } => {
            ensure!(
                config::safe_name(&name) && name != "control-token",
                "Invalid credential name"
            );
            let value = std::fs::read(value_file)?;
            ensure!(
                !value.is_empty() && value.len() < 65536,
                "Invalid credential"
            );
            config::private_write(&home.join("secrets").join(name), &value)?;
            println!("Credential stored");
        }
    }
    Ok(())
}
async fn api(home: &Path, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    desktop::local_api(home, method, path, body).await
}
fn generate_tls(home: &Path) -> Result<(PathBuf, PathBuf, Option<PathBuf>)> {
    use rcgen::{
        BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose,
        IsCa, Issuer, KeyPair, KeyUsagePurpose,
    };
    config::private_dir(&home.join("tls"))?;
    let mut ca = CertificateParams::new(vec![])?;
    let mut issuer_name = DistinguishedName::new();
    issuer_name.push(
        DnType::CommonName,
        format!("Agent Observatory CA {}", uuid::Uuid::new_v4()),
    );
    ca.distinguished_name = issuer_name;
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_key = KeyPair::generate()?;
    let ca_cert = ca.self_signed(&ca_key)?;
    config::private_write(&home.join("tls/root-ca.crt"), ca_cert.pem().as_bytes())?;
    config::private_write(
        &home.join("tls/root-ca.key"),
        ca_key.serialize_pem().as_bytes(),
    )?;
    let issuer = Issuer::new(ca, ca_key);
    let mut names = vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
    ];
    if let Ok(addresses) = if_addrs::get_if_addrs() {
        for a in addresses {
            let ip = a.ip().to_string();
            if !names.contains(&ip) {
                names.push(ip);
            }
        }
    }
    let mut leaf = CertificateParams::new(names)?;
    let mut service_name = DistinguishedName::new();
    service_name.push(DnType::CommonName, "Agent Observatory service");
    leaf.distinguished_name = service_name;
    leaf.use_authority_key_identifier_extension = true;
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let key = KeyPair::generate()?;
    let cert = leaf.signed_by(&key, &issuer)?;
    config::private_write(&home.join("tls/cert.pem"), cert.pem().as_bytes())?;
    config::private_write(&home.join("tls/key.pem"), key.serialize_pem().as_bytes())?;
    Ok((
        home.join("tls/cert.pem"),
        home.join("tls/key.pem"),
        Some(home.join("tls/root-ca.crt")),
    ))
}
