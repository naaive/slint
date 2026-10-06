// SPDX-License-Identifier: MIT

//! `nimbus-portal`: the Nimbus `xdg-desktop-portal` backend, started by D-Bus activation.

use anyhow::Context as _;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config_path = nimbus_config::default_path()?;
    let connection = zbus::Connection::session().await.context("connecting to the session bus")?;
    let _portal = nimbus_portal::serve(&connection, &config_path).await?;
    std::future::pending().await
}
