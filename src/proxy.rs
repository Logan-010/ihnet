use super::quinn_endpoint;
use anyhow::Context;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::client::conn::http1::Builder;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use iroh::{Endpoint, NodeAddr};
use std::str::FromStr;
use std::sync::OnceLock;

pub const TCP_ALPN: &[u8] = b"INETTCPV0";
pub const UDP_ALPN: &[u8] = b"INETUDPV0";
pub const HANDSHAKE: &[u8] = b"wyll";

pub static ENDPOINT: OnceLock<Endpoint> = OnceLock::new();
fn endpoint() -> &'static Endpoint {
    ENDPOINT.get().unwrap()
}

fn bad_request(text: &'static str) -> anyhow::Result<Response<BoxBody<Bytes, hyper::Error>>> {
    let mut resp: Response<BoxBody<Bytes, hyper::Error>> = Response::new(full(text));
    *resp.status_mut() = http::StatusCode::BAD_REQUEST;
    Ok(resp)
}

fn parse_subdomain(subdomain: &str) -> anyhow::Result<NodeAddr> {
    if let Ok(node_id) = iroh::NodeId::from_str(subdomain) {
        return Ok(NodeAddr {
            node_id,
            relay_url: None, // Use discovery
            direct_addresses: Default::default(),
        });
    }
    Err(anyhow::anyhow!("invalid subdomain"))
}

pub async fn proxy(
    req: Request<hyper::body::Incoming>,
) -> anyhow::Result<Response<BoxBody<Bytes, hyper::Error>>> {
    let (_, hostname) = req
        .headers()
        .iter()
        .find(|(name, _value)| name.as_str() == "host")
        .context("missing host header")
        .context(http::StatusCode::BAD_REQUEST)?;
    let Ok(hostname) = hostname.to_str() else {
        return bad_request("invalid host header - not ascii");
    };
    let parts = hostname.split('.').collect::<Vec<_>>();
    if parts.len() < 2 {
        return bad_request("invalid host header - missing subdomain");
    }
    let Ok(node_addr) = parse_subdomain(parts[0]) else {
        return bad_request("invalid host header - subdomain is neither a node id nor a ticket");
    };
    tracing::info!("connecting to node {:?}", node_addr);
    let conn = endpoint().connect(node_addr, TCP_ALPN).await?;
    tracing::info!("opening bi stream");
    let (mut send, recv) = conn.open_bi().await?;
    tracing::info!("sending handshake");
    send.write_all(HANDSHAKE).await?;
    let stream = quinn_endpoint::QuinnEndpoint { send, recv };
    let io = TokioIo::new(stream);

    let (mut sender, conn) = Builder::new()
        .preserve_header_case(true)
        .title_case_headers(true)
        .handshake(io)
        .await?;
    tokio::task::spawn(async move {
        if let Err(err) = conn.await {
            tracing::warn!("connection failed: {:?}", err);
        }
    });

    let resp = sender.send_request(req).await?;
    Ok(resp.map(|b| b.boxed()))
}

fn full<T: Into<Bytes>>(chunk: T) -> BoxBody<Bytes, hyper::Error> {
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed()
}
