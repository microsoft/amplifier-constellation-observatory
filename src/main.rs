use agent_observatory::{
    config::{self, Config},
    server,
    state::AppState,
};
use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
#[derive(Parser)]
#[command(version, about = "Observe and manage independent ACP agents")]
struct Args {
    #[arg(long, env = "OBSERVATORY_HOME")]
    home: PathBuf,
    #[command(subcommand)]
    command: Command,
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
    },
    Serve,
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
    match args.command {
        Command::Init {
            owner,
            bind,
            port,
            public_url,
            pam_service,
        } => {
            config::private_dir(&args.home)?;
            ensure!(
                !args.home.join("config.json").exists(),
                "An installation already exists here"
            );
            let (tls_cert, tls_key, tls_ca) = generate_tls(&args.home)?;
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
                public_url,
                pam_service,
                tls_cert,
                tls_key,
                tls_ca,
                embed_origins: vec![],
            };
            c.save(&args.home)?;
            AppState::open(args.home.clone())?;
            println!(
                "{}",
                json!({"home":args.home,"id":c.id,"port":c.port,"url":if c.public_url.is_empty(){format!("https://localhost:{}",c.port)}else{c.public_url}})
            );
        }
        Command::Serve => {
            use fs2::FileExt;
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(args.home.join("service.lock"))?;
            lock.try_lock_exclusive()?;
            let s = AppState::open(args.home.clone())?;
            let weak = std::sync::Arc::downgrade(&s);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    let Some(state) = weak.upgrade() else {
                        break;
                    };
                    agent_observatory::acp::evict_idle(&state);
                }
            });
            let address: std::net::SocketAddr =
                format!("{}:{}", s.config.bind, s.config.port).parse()?;
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
                &s.config.tls_cert,
                &s.config.tls_key,
            )
            .await?;
            config::private_write(&args.home.join("runtime.json"),serde_json::to_vec(&json!({"pid":std::process::id(),"port":s.config.port,"url":s.config.public_url,"version":env!("CARGO_PKG_VERSION")}))?.as_slice())?;
            axum_server::bind_rustls(address, tls)
                .serve(server::router(s).into_make_service())
                .await?;
        }
        Command::Config => println!(
            "{}",
            serde_json::to_string_pretty(&Config::load(&args.home)?)?
        ),
        Command::Status => println!(
            "{}",
            serde_json::to_string_pretty(&api(&args.home, "GET", "status", None).await?)?
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
                serde_json::to_string_pretty(&api(&args.home, &method, &path, body).await?)?
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
            config::private_write(&args.home.join("secrets").join(name), &value)?;
            println!("Credential stored");
        }
    }
    Ok(())
}
async fn api(home: &Path, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    ensure!(
        !path.starts_with('/') && !path.contains(".."),
        "Use a relative API path"
    );
    let c = Config::load(home)?;
    let ca = std::fs::read(c.tls_ca.as_ref().unwrap_or(&c.tls_cert))?;
    let client = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(&ca)?)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let token = std::fs::read_to_string(home.join("secrets/control-token"))?;
    let mut request = client
        .request(
            method.parse()?,
            format!("https://127.0.0.1:{}/api/v1/{path}", c.port),
        )
        .bearer_auth(token.trim());
    if let Some(b) = body {
        request = request.json(&b);
    }
    let response = request.send().await?;
    let status = response.status();
    let v: Value = response.json().await?;
    ensure!(
        status.is_success(),
        "{}",
        v["error"].as_str().unwrap_or("Request failed")
    );
    Ok(v)
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
    if let Ok(addresses) = get_if_addrs::get_if_addrs() {
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
