//! Happy Eyeballs (RFC 8305) TCP connect: a dual-stack hostname whose server
//! listens on one address family must not stall on the other family's timeout.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::{lookup_host, TcpStream};
use tokio::task::JoinSet;
use tokio::time::sleep;

const ATTEMPT_DELAY: Duration = Duration::from_millis(250);

pub async fn connect(host: &str, port: u16) -> io::Result<TcpStream> {
    let addrs = interleave_families(lookup_host((host, port)).await?.collect());
    race(addrs, ATTEMPT_DELAY).await
}

fn interleave_families(addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let first_is_v6 = addrs.first().is_some_and(SocketAddr::is_ipv6);
    let (preferred, other): (Vec<_>, Vec<_>) =
        addrs.into_iter().partition(|a| a.is_ipv6() == first_is_v6);
    let len = preferred.len().max(other.len());
    (0..len)
        .flat_map(|i| [preferred.get(i), other.get(i)])
        .flatten()
        .copied()
        .collect()
}

async fn race(addrs: Vec<SocketAddr>, delay: Duration) -> io::Result<TcpStream> {
    let mut pending = addrs.into_iter().peekable();
    let mut attempts = JoinSet::new();
    let mut last_err = io::Error::new(io::ErrorKind::NotFound, "host resolved to no addresses");
    loop {
        if attempts.is_empty() {
            match pending.next() {
                Some(addr) => drop(attempts.spawn(TcpStream::connect(addr))),
                None => return Err(last_err),
            }
        }
        let finished = tokio::select! {
            Some(res) = attempts.join_next() => Some(res),
            _ = sleep(delay), if pending.peek().is_some() => None,
        };
        match finished {
            Some(Ok(Ok(stream))) => return Ok(stream),
            Some(Ok(Err(e))) => last_err = e,
            Some(Err(e)) => last_err = io::Error::other(e),
            None => drop(attempts.spawn(TcpStream::connect(pending.next().expect("peeked")))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::time::Instant;
    use tokio::net::TcpListener;

    fn v4(last: u8) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, last)), 1)
    }

    fn v6(last: u16) -> SocketAddr {
        SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, last)),
            1,
        )
    }

    #[test]
    fn interleaves_starting_with_first_family() {
        assert_eq!(
            interleave_families(vec![v6(1), v6(2), v6(3), v4(1)]),
            vec![v6(1), v4(1), v6(2), v6(3)]
        );
    }

    #[tokio::test]
    async fn unresponsive_first_address_does_not_block_a_live_one() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live = listener.local_addr().unwrap();
        let blackhole = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), live.port());

        let started = Instant::now();
        let stream = race(vec![blackhole, live], Duration::from_millis(50))
            .await
            .unwrap();

        assert_eq!(stream.peer_addr().unwrap(), live);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn all_failures_return_the_last_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed = listener.local_addr().unwrap();
        drop(listener);

        assert!(race(vec![closed], Duration::from_millis(50)).await.is_err());
    }
}
