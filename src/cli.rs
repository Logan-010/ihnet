use clap::{Parser, Subcommand};
use iroh::NodeId;
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = env!("CARGO_PKG_DESCRIPTION"))]
#[command(author = env!("CARGO_PKG_AUTHORS"))]
pub struct Cli {
    /// Sets custom logging level
    #[arg(long, required = false, env = "RUST_LOG", default_value_t = String::from("ihnet=info"))]
    pub logging: String,

    /// Sets default port to listen on
    #[arg(long, required = false, default_value_t = 61608)]
    pub port: u16,

    /// Enables ipv6
    #[arg(long, required = false, default_value_t = false)]
    pub ipv6: bool,

    /// Path to save or load identity from
    #[arg(long, required =false, default_value = None)]
    pub identity: Option<PathBuf>,

    #[clap(subcommand)]
    pub mode: Mode,
}

#[derive(Clone, Subcommand)]
pub enum Mode {
    /// Forward local (TCP/UDP) port to network
    Forward {
        /// Local address to forward
        addr: SocketAddr,
    },
    /// Forward local TCP port to network
    ForwardTcp {
        /// Local address to forward
        addr: SocketAddr,
    },
    /// Forward local UDP port to network
    ForwardUdp {
        /// Local address to forward
        addr: SocketAddr,
    },
    /// Connect to forwarded TCP and UDP socket
    Connect {
        /// Node id to connect to
        to: NodeId,
    },
    /// Connect to forwarded TCP socket
    ConnectTcp {
        /// Node id to connect to
        to: NodeId,
    },
    /// Connect to forwarded UDP socket
    ConnectUdp {
        /// Node id to connect to
        to: NodeId,
    },
    /// Connects to network with web interface
    Web,
}
