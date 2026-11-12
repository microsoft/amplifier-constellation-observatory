#![cfg_attr(windows, windows_subsystem = "windows")]
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use constellation_machine_helper::{
    app::{self, App},
    client::Bootstrap,
    platform,
    storage::Store,
};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc, time::Duration};

#[derive(Parser)]
#[command(version, about = "Connect this computer to your Observatory")]
struct Cli {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    bootstrap: Option<PathBuf>,
    #[arg(long)]
    no_browser: bool,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    Run,
    Setup,
}
#[derive(Serialize, Deserialize)]
struct LocalSession {
    origin: String,
    token: String,
}
#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let show_error = !cli.no_browser && !matches!(cli.command, Some(Action::Run));
    if let Err(error) = run(cli).await {
        eprintln!("Machine helper: {error:#}");
        if show_error {
            platform::show_launch_error(&format!(
                "Setup could not open: {error:#}\n\nYour agents have not been changed. Check this computer's access, then open the setup download from your Observatory again."
            ));
        }
        std::process::exit(1)
    }
}
async fn run(cli: Cli) -> Result<()> {
    platform::ensure_supported()?;
    let background = matches!(cli.command, Some(Action::Run));
    let bootstrap_path = cli
        .bootstrap
        .or_else(|| {
            if !background {
                std::env::current_exe()
                    .ok()
                    .and_then(|exe| platform::bootstrap_path(&exe))
            } else {
                None
            }
        })
        .or_else(|| cli.home.as_ref().map(|h| h.join("observatory.json")));
    let Some(path) = bootstrap_path else {
        return no_bootstrap(cli.no_browser).await;
    };
    let raw = std::fs::read(path)
        .context("Download setup from your Observatory; its connection file is missing")?;
    ensure!(raw.len() <= 131072, "Setup file is too large");
    let bootstrap: Bootstrap = serde_json::from_slice(&raw)
        .context("Use the unchanged setup download from Observatory")?;
    bootstrap.validate()?;
    let home = match cli.home {
        Some(home) => home,
        None => platform::default_home(&bootstrap.observatory_id)?,
    };
    let store = Store::open(home)?;
    let lock = match store.lock() {
        Ok(lock) => lock,
        Err(error) => {
            if background {
                return Err(error);
            }
            let local: LocalSession = store
                .read("local-session.json")?
                .context("The helper is already running; its setup page is not ready yet")?;
            let url = reqwest::Url::parse(&local.origin)?;
            ensure!(
                url.scheme() == "http"
                    && url.host_str() == Some("127.0.0.1")
                    && url.port().is_some()
                    && url.path() == "/",
                "Invalid local helper address"
            );
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .build()?;
            client
                .get(format!("{}/api/status", local.origin))
                .bearer_auth(&local.token)
                .send()
                .await?
                .error_for_status()?;
            let _ = client
                .post(format!("{}/api/bootstrap", local.origin))
                .header("Origin", &local.origin)
                .bearer_auth(&local.token)
                .json(&bootstrap)
                .send()
                .await?;
            if !cli.no_browser {
                platform::open_browser(&format!("{}/#{}", local.origin, local.token))?;
            }
            return Ok(());
        }
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let app = Arc::new(App::open(store, bootstrap, origin.clone())?);
    app.store.save(
        "local-session.json",
        &LocalSession {
            origin: origin.clone(),
            token: app.token.clone(),
        },
    )?;
    let heartbeat = tokio::spawn(app.clone().heartbeat_loop());
    let browser_url = format!("{origin}/#{}", app.token);
    if !background
        && !cli.no_browser
        && let Err(error) = platform::open_browser(&browser_url)
    {
        eprintln!("{error}. Open: {browser_url}");
    }
    if cli.no_browser {
        println!("{browser_url}");
    }
    let server = axum::serve(listener, app::router(app)).with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    let result = server.await;
    heartbeat.abort();
    drop(lock);
    result?;
    Ok(())
}
async fn no_bootstrap(no_browser: bool) -> Result<()> {
    use axum::{Router, response::Html, routing::get};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let router=Router::new().route("/",get(||async{Html("<!doctype html><html lang=en><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>Constellation Computer</title><h1>Start from your Observatory.</h1><p>In Observatory, choose Add a computer and download setup for this computer. Open that download to enter your pairing code.</p><p>This app has not connected or changed anything.</p></html>")}));
    if no_browser {
        println!("{origin}")
    } else {
        platform::open_browser(&origin)?;
    }
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            tokio::time::sleep(Duration::from_secs(300)).await;
        })
        .await?;
    Ok(())
}
