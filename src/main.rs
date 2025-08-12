use anyhow::bail;
use clap::Parser;
use cli::{Cli, Mode};
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use iroh::{Endpoint, NodeId, SecretKey, endpoint::Connection};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use tokio::{
    fs, io, join,
    net::{TcpListener, TcpStream, UdpSocket},
    task,
};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

mod cli;
mod proxy;
mod quinn_endpoint;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if tracing_subscriber::registry()
        .with(EnvFilter::new(cli.logging))
        .with(tracing_subscriber::fmt::layer())
        .try_init()
        .is_err()
    {
        eprintln!("Failed to initialize logger");
    }

    let secret = match cli.identity {
        Some(path) => {
            if path.exists() {
                let data = fs::read(path).await?;

                let mut key = [0u8; 32];
                key.clone_from_slice(&data[..32]);

                SecretKey::from_bytes(&key)
            } else {
                let key = SecretKey::generate(rand::thread_rng());

                fs::write(path, &key.to_bytes()).await?;

                key
            }
        }
        None => SecretKey::generate(rand::thread_rng()),
    };

    let endpoint_builder = Endpoint::builder()
        .secret_key(secret)
        .discovery_n0()
        .discovery_dht()
        .discovery_local_network();

    match cli.mode {
        Mode::ForwardTcp { addr } => {
            let endpoint = endpoint_builder
                .alpns(vec![proxy::TCP_ALPN.to_vec()])
                .bind()
                .await?;

            tracing::info!("my id {}", endpoint.node_id());

            while let Some(connecting) = endpoint.accept().await {
                tracing::info!("peer connecting");
                if let Ok(conn) = connecting.await {
                    tracing::info!(
                        "peer {} connected",
                        conn.remote_node_id()
                            .map(|n| n.to_string())
                            .unwrap_or(String::from("?"))
                    );

                    let Some(alpn) = conn.alpn() else {
                        tracing::warn!("expected alpn");
                        continue;
                    };

                    if alpn == proxy::TCP_ALPN {
                        task::spawn(async move {
                            if let Err(e) = forward_tcp(conn, addr).await {
                                tracing::warn!("failed to serve connection: {}", e);
                            } else {
                                tracing::info!("served connection");
                            }
                        });
                    }
                } else {
                    tracing::warn!("error accepting conneciton");
                }
            }

            Ok(())
        }
        Mode::ForwardUdp { addr } => {
            let endpoint = endpoint_builder
                .alpns(vec![proxy::UDP_ALPN.to_vec()])
                .bind()
                .await?;

            tracing::info!("my id {}", endpoint.node_id());

            while let Some(connecting) = endpoint.accept().await {
                tracing::info!("peer connecting");
                if let Ok(conn) = connecting.await {
                    tracing::info!(
                        "peer {} connected",
                        conn.remote_node_id()
                            .map(|n| n.to_string())
                            .unwrap_or(String::from("?"))
                    );

                    let Some(alpn) = conn.alpn() else {
                        tracing::warn!("expected alpn");
                        continue;
                    };

                    if alpn == proxy::UDP_ALPN {
                        task::spawn(async move {
                            if let Err(e) = forward_udp(conn, addr).await {
                                tracing::warn!("failed to serve connection: {}", e);
                            } else {
                                tracing::info!("served connection");
                            }
                        });
                    }
                } else {
                    tracing::warn!("error accepting conneciton");
                }
            }

            Ok(())
        }
        Mode::Forward { addr } => {
            let endpoint = endpoint_builder
                .alpns(vec![proxy::TCP_ALPN.to_vec(), proxy::UDP_ALPN.to_vec()])
                .bind()
                .await?;

            tracing::info!("my id {}", endpoint.node_id());

            while let Some(connecting) = endpoint.accept().await {
                tracing::info!("peer connecting");
                if let Ok(conn) = connecting.await {
                    tracing::info!(
                        "peer {} connected",
                        conn.remote_node_id()
                            .map(|n| n.to_string())
                            .unwrap_or(String::from("?"))
                    );

                    let Some(alpn) = conn.alpn() else {
                        tracing::warn!("expected alpn");
                        continue;
                    };

                    if alpn == proxy::TCP_ALPN {
                        tracing::info!("forwarding tcp peer");
                        task::spawn(async move {
                            if let Err(e) = forward_tcp(conn, addr).await {
                                tracing::warn!("failed to serve connection: {}", e);
                            } else {
                                tracing::info!("served connection");
                            }
                        });
                    } else if alpn == proxy::UDP_ALPN {
                        tracing::info!("forwarding udp peer");
                        task::spawn(async move {
                            if let Err(e) = forward_udp(conn, addr).await {
                                tracing::warn!("failed to serve connection: {}", e);
                            } else {
                                tracing::info!("served connection");
                            }
                        });
                    }
                } else {
                    tracing::warn!("error accepting conneciton");
                }
            }

            Ok(())
        }
        Mode::ConnectTcp { to } => {
            let tcp_listener = TcpListener::bind(cli.address).await?;
            let endpoint = endpoint_builder.bind().await?;

            tracing::info!(
                "listening for tcp connectons on {}",
                tcp_listener.local_addr()?
            );

            loop {
                match tcp_listener.accept().await {
                    Ok((stream, _)) => {
                        tracing::info!("accepted local request");

                        if let Err(e) = connect_tcp(&endpoint, stream, to).await {
                            tracing::warn!("failed to forward local request {}", e);
                            continue;
                        }
                    }
                    Err(e) => tracing::warn!("failed to accept connection {}", e),
                }
            }
        }
        Mode::ConnectUdp { to } => {
            let udp_listener = UdpSocket::bind(cli.address).await?;
            let endpoint = endpoint_builder.bind().await?;

            tracing::info!(
                "listening for udp connections on {}",
                udp_listener.local_addr()?
            );

            loop {
                if let Err(e) = connect_udp(&endpoint, &udp_listener, to).await {
                    tracing::warn!("failed to connect to forwarded udp socket {}", e);
                    continue;
                }
            }
        }
        Mode::Connect { to } => {
            let udp_listener = UdpSocket::bind(cli.address).await?;
            let tcp_listener = TcpListener::bind(cli.address).await?;
            let endpoint = endpoint_builder.bind().await?;
            let endpoint_clone = endpoint.clone();

            tracing::info!(
                "listening for tcp connections on {}, and udp connections on {}",
                tcp_listener.local_addr()?,
                udp_listener.local_addr()?
            );

            let udp_task = task::spawn(async move {
                loop {
                    if let Err(e) = connect_udp(&endpoint_clone, &udp_listener, to).await {
                        tracing::warn!("failed to connect to forwarded udp socket {}", e);
                        continue;
                    }
                }
            });

            let tcp_task = task::spawn(async move {
                loop {
                    match tcp_listener.accept().await {
                        Ok((stream, _)) => {
                            tracing::info!("accepted local request");

                            if let Err(e) = connect_tcp(&endpoint, stream, to).await {
                                tracing::warn!("failed to forward local request {}", e);
                                continue;
                            }
                        }
                        Err(e) => tracing::warn!("failed to accept connection {}", e),
                    }
                }
            });

            _ = join!(udp_task, tcp_task);

            Ok(())
        }
        Mode::Web => {
            let listener = TcpListener::bind(cli.address).await?;
            let endpoint = endpoint_builder.bind().await?;
            proxy::ENDPOINT.set(endpoint).unwrap();

            tracing::info!("listening on {}", listener.local_addr()?);

            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("failed to accept connection: {}", e);
                        continue;
                    }
                };

                tracing::info!("accepted local request");

                task::spawn(async move {
                    if let Err(err) = http1::Builder::new()
                        .preserve_header_case(true)
                        .title_case_headers(true)
                        .serve_connection(TokioIo::new(stream), service_fn(proxy::proxy))
                        .with_upgrades()
                        .await
                    {
                        tracing::warn!("failed to serve connection: {:?}", err);
                    }
                });
            }
        }
    }
}

async fn connect_tcp(
    endpoint: &Endpoint,
    mut local_stream: TcpStream,
    to: NodeId,
) -> anyhow::Result<()> {
    let conn = endpoint.connect(to, proxy::TCP_ALPN).await?;
    let (mut send, recv) = conn.open_bi().await?;
    send.write_all(proxy::HANDSHAKE).await?;

    tracing::info!("opened stream");

    let mut stream = quinn_endpoint::QuinnEndpoint { send, recv };

    io::copy_bidirectional(&mut local_stream, &mut stream).await?;

    tracing::info!("copied stream");

    conn.closed().await;

    Ok(())
}

async fn connect_udp(endpoint: &Endpoint, socket: &UdpSocket, to: NodeId) -> anyhow::Result<()> {
    let conn = endpoint.connect(to, proxy::UDP_ALPN).await?;

    let (mut send, mut recv) = conn.open_bi().await?;
    send.write_all(proxy::HANDSHAKE).await?;

    let mut buf = [0u8; 1500];
    loop {
        let (read, from) = socket.recv_from(&mut buf).await?;

        send.write_all(&buf[..read]).await?;

        let read_back = recv.read(&mut buf).await?.unwrap_or(0);

        socket.send_to(&buf[..read_back], from).await?;
    }
}

async fn forward_tcp(conn: Connection, addr: SocketAddr) -> anyhow::Result<()> {
    let (send, mut recv) = conn.accept_bi().await?;

    let mut handshake = [0u8; proxy::HANDSHAKE.len()];
    recv.read_exact(&mut handshake).await?;

    if handshake != proxy::HANDSHAKE {
        bail!("Invalid handshake");
    }

    let mut stream = quinn_endpoint::QuinnEndpoint { send, recv };

    let mut local_stream = TcpStream::connect(addr).await?;

    io::copy_bidirectional(&mut stream, &mut local_stream).await?;

    conn.close(0u32.into(), b"bye");

    Ok(())
}

async fn forward_udp(conn: Connection, addr: SocketAddr) -> anyhow::Result<()> {
    let (mut send, mut recv) = conn.accept_bi().await?;

    let mut handshake = [0u8; proxy::HANDSHAKE.len()];
    recv.read_exact(&mut handshake).await?;

    if handshake != proxy::HANDSHAKE {
        bail!("Invalid handshake");
    }

    let mut buf = [0u8; 1500];
    let bind_addr = match addr.is_ipv6() {
        true => IpAddr::V6(Ipv6Addr::LOCALHOST),
        false => IpAddr::V4(Ipv4Addr::LOCALHOST),
    };
    let socket = UdpSocket::bind(SocketAddr::new(bind_addr, 0)).await?;
    socket.connect(addr).await?;

    loop {
        let read = recv.read(&mut buf).await?.unwrap_or(0);

        socket.send(&buf[..read]).await?;

        let read_back = socket.recv(&mut buf).await?;

        send.write_all(&buf[..read_back]).await?;
    }
}
