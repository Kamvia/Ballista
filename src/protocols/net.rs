// File: ballista/src/protocols/net.rs
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

use super::host_allowed;
use crate::error::{Error, Result, sanitize};
use crate::vu::Vu;

const MAX_READ: usize = 16 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;

/// How much of a TCP response to read.
#[derive(Debug, Clone)]
pub enum TcpRead {
    /// Nothing; measure connect and write only.
    None,
    /// Whatever the first read returns.
    First,
    /// Exactly this many bytes.
    Bytes(usize),
    /// Until this delimiter (inclusive).
    Until(Vec<u8>),
    /// Until the peer closes the connection.
    Close,
}

fn check_addr(vu: &Vu, addr: &str) -> Result<()> {
    if addr.len() > 300 || !addr.contains(':') {
        return Err(Error::check("address must be host:port"));
    }
    if !host_allowed(vu.plan(), addr) {
        return Err(Error::check(format!("host of '{}' is not in target.allowed_hosts", sanitize(addr, 80))));
    }
    Ok(())
}

async fn tcp_inner(addr: &str, payload: &[u8], read: &TcpRead) -> std::io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect(addr).await?;
    stream.set_nodelay(true)?;
    if !payload.is_empty() {
        stream.write_all(payload).await?;
    }
    let mut out = Vec::new();
    let mut buf = vec![0_u8; CHUNK];
    match read {
        TcpRead::None => {}
        TcpRead::First => {
            let n = stream.read(&mut buf).await?;
            out.extend_from_slice(&buf[..n]);
        }
        TcpRead::Bytes(want) => {
            let want = (*want).min(MAX_READ);
            out.resize(want, 0);
            stream.read_exact(&mut out).await?;
        }
        TcpRead::Until(delim) => loop {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "closed before delimiter"));
            }
            out.extend_from_slice(&buf[..n]);
            if out.windows(delim.len().max(1)).any(|w| w == delim.as_slice()) {
                break;
            }
            if out.len() > MAX_READ {
                return Err(std::io::Error::other("response exceeds 16 MiB"));
            }
        },
        TcpRead::Close => loop {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            if out.len() > MAX_READ {
                return Err(std::io::Error::other("response exceeds 16 MiB"));
            }
        },
    }
    Ok(out)
}

impl Vu {
    /// Connects to `addr`, writes `payload`, reads per `read` and records the exchange as `TCP name`.
    pub async fn tcp_exchange(&self, addr: &str, payload: &[u8], read: TcpRead, timeout: Duration, name: &str) -> Result<Vec<u8>> {
        check_addr(self, addr)?;
        self.measure("TCP", name, async {
            tokio::time::timeout(timeout, tcp_inner(addr, payload, &read))
                .await
                .map_err(|_| "timeout".to_owned())?
                .map_err(|e| e.to_string())
        })
        .await
    }

    /// Sends one datagram and optionally waits for one reply; recorded as `UDP name`.
    pub async fn udp_exchange(&self, addr: &str, payload: &[u8], expect_reply: bool, timeout: Duration, name: &str) -> Result<Vec<u8>> {
        check_addr(self, addr)?;
        self.measure("UDP", name, async {
            let fut = async {
                let target = tokio::net::lookup_host(addr).await?.next().ok_or_else(|| std::io::Error::other("address did not resolve"))?;
                let bind = if target.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
                let sock = UdpSocket::bind(bind).await?;
                sock.connect(target).await?;
                sock.send(payload).await?;
                let mut buf = vec![0_u8; 65_535];
                if expect_reply {
                    let n = sock.recv(&mut buf).await?;
                    buf.truncate(n);
                    Ok::<_, std::io::Error>(buf)
                } else {
                    Ok(Vec::new())
                }
            };
            tokio::time::timeout(timeout, fut).await.map_err(|_| "timeout".to_owned())?.map_err(|e| e.to_string())
        })
        .await
    }
}
