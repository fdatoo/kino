//! Bonjour/mDNS service discovery.

use std::{future::Future, io, time::Duration};

use kino_core::Id;
use mdns_sd::{DaemonStatus, ServiceDaemon, ServiceInfo, UnregisterStatus};
use tokio::{sync::oneshot, task::JoinHandle};
use tracing::{debug, error, info};

const SERVICE_TYPE: &str = "_kino._tcp.local.";
const API_VERSION: &str = "v1";
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Runtime mDNS advertisement configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryConfig {
    /// Whether mDNS discovery is enabled.
    pub enabled: bool,
    /// Bonjour service instance name.
    pub instance_name: String,
    /// TCP port advertised for the Kino HTTP API.
    pub port: u16,
    /// Kino server semver advertised in TXT records.
    pub version: String,
    /// Stable server-side installation identity advertised in TXT records.
    pub instance_id: Id,
    host_name: String,
}

/// Errors produced by mDNS discovery setup and shutdown.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Creating or controlling the mDNS daemon failed.
    #[error("mdns discovery failed: {0}")]
    Mdns(#[from] mdns_sd::Error),

    /// Reading the system hostname failed.
    #[error("mdns discovery hostname lookup failed: {0}")]
    Hostname(#[source] io::Error),

    /// The mDNS daemon did not complete a lifecycle operation.
    #[error("mdns discovery daemon error: {0}")]
    Daemon(String),
}

/// Crate-local result alias for mDNS discovery.
pub type Result<T> = std::result::Result<T, Error>;

impl DiscoveryConfig {
    /// Build discovery config from persisted config and runtime values.
    pub fn from_config_and_runtime(
        config: &kino_core::config::DiscoveryConfig,
        port: u16,
        version: impl Into<String>,
        instance_id: Id,
    ) -> Result<Self> {
        let hostname = if config.enabled {
            Some(system_hostname()?)
        } else {
            None
        };
        let instance_name = config
            .instance_name
            .clone()
            .or_else(|| hostname.clone())
            .unwrap_or_else(|| "kino".to_owned());
        let host_name = service_host_name(hostname.as_deref().unwrap_or(&instance_name));

        Ok(Self {
            enabled: config.enabled,
            instance_name,
            port,
            version: version.into(),
            instance_id,
            host_name,
        })
    }
}

/// Start the mDNS discovery task.
pub fn spawn(config: DiscoveryConfig) -> JoinHandle<()> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _shutdown_tx = shutdown_tx;
        if let Err(err) = run_until_shutdown(config, async move {
            let _ = shutdown_rx.await;
        })
        .await
        {
            error!(error = %err, "mdns discovery stopped");
        }
    })
}

async fn run_until_shutdown<S>(config: DiscoveryConfig, shutdown: S) -> Result<()>
where
    S: Future<Output = ()> + Send,
{
    if !config.enabled {
        info!("mdns discovery disabled");
        return Ok(());
    }

    let service_info = service_info(&config)?;
    let service_fullname = service_info.get_fullname().to_owned();
    let daemon = ServiceDaemon::new()?;
    daemon.register(service_info)?;
    info!(
        name = %config.instance_name,
        service = %service_fullname,
        port = config.port,
        "mdns discovery registered"
    );

    shutdown.await;

    info!(service = %service_fullname, "mdns discovery shutting down");
    unregister(&daemon, &service_fullname)?;
    shutdown_daemon(&daemon)?;
    info!("mdns discovery stopped");
    Ok(())
}

fn service_info(config: &DiscoveryConfig) -> Result<ServiceInfo> {
    let instance_id = config.instance_id.to_string();
    let properties = [
        ("version", config.version.as_str()),
        ("api", API_VERSION),
        ("instance_id", instance_id.as_str()),
    ];

    Ok(ServiceInfo::new(
        SERVICE_TYPE,
        &config.instance_name,
        &config.host_name,
        "",
        config.port,
        &properties[..],
    )?
    .enable_addr_auto())
}

fn unregister(daemon: &ServiceDaemon, service_fullname: &str) -> Result<()> {
    let receiver = daemon.unregister(service_fullname)?;
    match receiver
        .recv_timeout(SHUTDOWN_TIMEOUT)
        .map_err(|err| Error::Daemon(format!("unregister status unavailable: {err}")))?
    {
        UnregisterStatus::OK => {}
        UnregisterStatus::NotFound => {
            debug!(service = %service_fullname, "mdns discovery service already unregistered");
        }
    }
    Ok(())
}

fn shutdown_daemon(daemon: &ServiceDaemon) -> Result<()> {
    let receiver = daemon.shutdown()?;
    match receiver
        .recv_timeout(SHUTDOWN_TIMEOUT)
        .map_err(|err| Error::Daemon(format!("shutdown status unavailable: {err}")))?
    {
        DaemonStatus::Shutdown => Ok(()),
        status => Err(Error::Daemon(format!(
            "unexpected shutdown status: {status:?}"
        ))),
    }
}

fn system_hostname() -> Result<String> {
    hostname::get()
        .map(|hostname| {
            let hostname = hostname.to_string_lossy().trim().to_owned();
            if hostname.is_empty() {
                "kino".to_owned()
            } else {
                hostname
            }
        })
        .map_err(Error::Hostname)
}

fn service_host_name(hostname: &str) -> String {
    let hostname = hostname.trim().trim_end_matches('.');
    let hostname = hostname.strip_suffix(".local").unwrap_or(hostname);
    if hostname.is_empty() {
        "kino.local.".to_owned()
    } else {
        format!("{hostname}.local.")
    }
}

#[cfg(test)]
fn spawn_with_shutdown(config: DiscoveryConfig, shutdown: oneshot::Receiver<()>) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(err) = run_until_shutdown(config, async move {
            let _ = shutdown.await;
        })
        .await
        {
            error!(error = %err, "mdns discovery stopped");
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::Duration;

    use mdns_sd::{ServiceDaemon, ServiceEvent};

    use super::*;

    #[test]
    fn config_defaults_instance_name_to_hostname()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let core = kino_core::config::DiscoveryConfig::default();
        let config = DiscoveryConfig::from_config_and_runtime(&core, 7777, "1.2.3", Id::new())?;

        assert!(!config.instance_name.is_empty());
        assert_eq!(config.port, 7777);
        assert_eq!(config.version, "1.2.3");
        Ok(())
    }

    #[test]
    fn service_host_name_adds_local_domain_once() {
        assert_eq!(service_host_name("kino"), "kino.local.");
        assert_eq!(service_host_name("kino.local"), "kino.local.");
        assert_eq!(service_host_name("kino.local."), "kino.local.");
    }

    #[tokio::test]
    #[cfg_attr(not(feature = "mdns-tests"), ignore)]
    async fn mdns_browser_resolves_advertised_service()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        // Runs locally on macOS, or on a Linux host with multicast support. It
        // is ignored by default because containerized CI often blocks mDNS.
        let instance_id = Id::new();
        let config = DiscoveryConfig {
            enabled: true,
            instance_name: format!("Kino Test {instance_id}"),
            port: 54321,
            version: "1.2.3".to_owned(),
            instance_id,
            host_name: service_host_name(&system_hostname()?),
        };
        let service_fullname = service_info(&config)?.get_fullname().to_owned();
        let browser_daemon = ServiceDaemon::new()?;
        let browser = browser_daemon.browse(SERVICE_TYPE)?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handle = spawn_with_shutdown(config, shutdown_rx);

        let resolved = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = browser.recv_async().await?;
                debug!(event = ?event, "mdns discovery test event");
                if let ServiceEvent::ServiceResolved(info) = event
                    && info.get_fullname() == service_fullname
                {
                    return Ok::<_, Box<dyn std::error::Error>>(info);
                }
            }
        })
        .await??;

        assert_eq!(resolved.get_port(), 54321);
        assert_eq!(resolved.get_property_val_str("version"), Some("1.2.3"));
        assert_eq!(resolved.get_property_val_str("api"), Some(API_VERSION));
        let expected_instance_id = instance_id.to_string();
        assert_eq!(
            resolved.get_property_val_str("instance_id"),
            Some(expected_instance_id.as_str())
        );

        shutdown_tx.send(()).unwrap();
        handle.await?;
        browser_daemon.shutdown()?;
        Ok(())
    }
}
