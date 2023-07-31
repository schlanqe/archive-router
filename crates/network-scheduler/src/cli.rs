use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use serde::{Deserialize, Serialize};
use subsquid_network_transport::cli::TransportArgs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub schedule_interval_sec: u64,
    pub replication_factor: usize,
    pub scheduling_unit_size: usize,
    pub worker_storage_bytes: u64,
    pub s3_endpoint: String,
    pub buckets: Vec<String>,
}

#[derive(Parser)]
pub struct Cli {
    #[command(flatten)]
    pub transport: TransportArgs,

    #[arg(
        long,
        env,
        help = "HTTP metrics server listen addr",
        default_value = "0.0.0.0:8000"
    )]
    pub http_listen_addr: SocketAddr,

    #[arg(
        long,
        env,
        help = "Blockchain RPC URL",
        default_value = "http://127.0.0.1:8545/"
    )]
    pub rpc_url: String,

    #[arg(
        long,
        env,
        help = "Path to save metrics. If not present, stdout is used."
    )]
    pub metrics_path: Option<PathBuf>,

    #[arg(
        long,
        env,
        help = "Choose which metrics should be printed.",
        value_delimiter = ',',
        num_args = 0..,
        default_value = "QuerySubmitted,QueryFinished,QueryExecuted,WorkersSnapshot"
    )]
    pub metrics: Vec<String>,

    #[arg(
        short,
        long,
        env = "CONFIG_PATH",
        help = "Path to config file",
        default_value = "config.yml"
    )]
    config: PathBuf,
}

impl Cli {
    pub async fn config(&self) -> anyhow::Result<Config> {
        let file_contents = tokio::fs::read(&self.config).await?;
        Ok(serde_yaml::from_slice(file_contents.as_slice())?)
    }
}
