use std::{any::Any, io, net::IpAddr};

use webrtc::{
    api::setting_engine::SettingEngine,
    ice::{
        network_type::NetworkType,
        udp_mux::{UDPMux, UDPMuxDefault, UDPMuxParams},
        udp_network::UDPNetwork,
    },
    ice_transport::ice_candidate_type::RTCIceCandidateType,
    util::{Conn, Result as ConnResult},
};

use super::*;

const MAX_DATAGRAM: usize = 2048;
const FRAME_SIZE: usize = MAX_DATAGRAM + 128;

pub struct IceTransport {
    mux: Arc<UDPMuxDefault>,
    ip: IpAddr,
}

impl IceTransport {
    pub fn settings(&self) -> SettingEngine {
        let mut settings = SettingEngine::default();
        // ICE uses the virtual address and port in SDP, but sends via the loopback UDP relay.
        settings.set_include_loopback_candidate(true);
        settings.set_ice_multicast_dns_mode(webrtc::ice::mdns::MulticastDnsMode::Disabled);
        settings.set_network_types(vec![NetworkType::Udp4]);
        settings.set_ip_filter(Box::new(|ip| ip == Ipv4Addr::LOCALHOST));
        settings.set_nat_1to1_ips(vec![self.ip.to_string()], RTCIceCandidateType::Host);
        settings.set_udp_network(UDPNetwork::Muxed(self.mux.clone()));
        settings
    }

    pub async fn close(self) {
        let _ = self.mux.close().await;
    }
}

impl EmbeddedTailnet {
    pub fn ice_transport(&self) -> Result<IceTransport> {
        let state = snapshot(&self.state_dir)?;
        ensure!(
            state.ready && state.relay != 0,
            "embedded Tailscale UDP is not ready"
        );
        let media: SocketAddr = state.media.parse()?;
        ensure!(media.is_ipv4() && tailnet::is_tailnet_ip(media.ip()));
        let relay = Relay {
            socket: self.udp.clone(),
            token: self.token.clone(),
            bridge: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), state.relay),
            media,
        };
        Ok(IceTransport {
            ip: media.ip(),
            mux: UDPMuxDefault::new(UDPMuxParams::new(relay)),
        })
    }
}

struct Relay {
    socket: Arc<UdpSocket>,
    token: String,
    bridge: SocketAddr,
    media: SocketAddr,
}

fn unconnected<T>() -> ConnResult<T> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "unconnected UDP relay").into())
}

fn unpack<'a>(frame: &'a [u8], token: &str) -> Option<(SocketAddr, &'a [u8])> {
    let (header, packet) = frame.split_at_checked(frame.iter().position(|&byte| byte == b'\n')?)?;
    let (candidate, address) = header.split_at_checked(token.len())?;
    if !bool::from(candidate.ct_eq(token.as_bytes())) {
        return None;
    }
    let peer: SocketAddr = std::str::from_utf8(address.strip_prefix(b" ")?)
        .ok()?
        .parse()
        .ok()?;
    let packet = &packet[1..];
    (peer.is_ipv4() && tailnet::is_tailnet_ip(peer.ip()) && packet.len() <= MAX_DATAGRAM)
        .then_some((peer, packet))
}

#[async_trait]
impl Conn for Relay {
    async fn connect(&self, _: SocketAddr) -> ConnResult<()> {
        unconnected()
    }

    async fn recv(&self, _: &mut [u8]) -> ConnResult<usize> {
        unconnected()
    }

    async fn recv_from(&self, buf: &mut [u8]) -> ConnResult<(usize, SocketAddr)> {
        let mut frame = [0u8; FRAME_SIZE];
        loop {
            self.socket.readable().await?;
            let (len, source) = match self.socket.try_recv_from(&mut frame) {
                Ok(result) => result,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                // Oversized loopback packets can yield WSAEMSGSIZE instead of truncation.
                #[cfg(windows)]
                Err(error) if error.raw_os_error() == Some(10040) => continue,
                Err(error) => return Err(error.into()),
            };
            if source == self.bridge
                && let Some((peer, packet)) = unpack(&frame[..len], &self.token)
                && packet.len() <= buf.len()
            {
                buf[..packet.len()].copy_from_slice(packet);
                return Ok((packet.len(), peer));
            }
            tokio::task::yield_now().await;
        }
    }

    async fn send(&self, _: &[u8]) -> ConnResult<usize> {
        unconnected()
    }

    async fn send_to(&self, packet: &[u8], target: SocketAddr) -> ConnResult<usize> {
        if !target.is_ipv4() || !tailnet::is_tailnet_ip(target.ip()) || packet.len() > MAX_DATAGRAM
        {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "invalid tailnet datagram").into(),
            );
        }
        use std::io::Write;
        let mut frame = [0u8; FRAME_SIZE];
        let mut cursor = io::Cursor::new(frame.as_mut_slice());
        writeln!(cursor, "{} {target}", self.token)?;
        Write::write_all(&mut cursor, packet)?;
        let len = cursor.position() as usize;
        self.socket.send_to(&frame[..len], self.bridge).await?;
        Ok(packet.len())
    }

    fn local_addr(&self) -> ConnResult<SocketAddr> {
        Ok(self.media)
    }

    fn remote_addr(&self) -> Option<SocketAddr> {
        None
    }

    async fn close(&self) -> ConnResult<()> {
        Ok(())
    }

    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use webrtc::{
        api::APIBuilder, peer_connection::RTCPeerConnection,
        peer_connection::configuration::RTCConfiguration,
    };

    #[tokio::test]
    async fn relay_preserves_datagrams_addresses_and_virtual_port() {
        let go = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let socket = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());
        let relay = Relay {
            socket: socket.clone(),
            token: "token".to_owned(),
            bridge: go.local_addr().unwrap(),
            media: "100.100.0.2:7337".parse().unwrap(),
        };
        let peer = "100.100.0.3:9000".parse().unwrap();
        let packet = b"\x00\nhello\xff";
        assert_eq!(relay.local_addr().unwrap(), relay.media);
        assert!(
            relay
                .send_to(packet, "192.168.1.1:443".parse().unwrap())
                .await
                .is_err()
        );
        assert_eq!(relay.send_to(packet, peer).await.unwrap(), packet.len());
        let mut buf = [0u8; FRAME_SIZE];
        let (len, rust) = go.recv_from(&mut buf).await.unwrap();
        assert_eq!(
            unpack(&buf[..len], "token"),
            Some((peer, packet.as_slice()))
        );
        go.send_to(b"bad-token 100.100.0.3:9000\ninjected", rust)
            .await
            .unwrap();
        go.send_to(&buf[..len], rust).await.unwrap();
        let (len, actual) = tokio::time::timeout(Duration::from_secs(2), relay.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(actual, peer);
        assert_eq!(&buf[..len], packet);
    }

    #[tokio::test]
    async fn negotiates_webrtc_using_only_virtual_tailnet_candidates() {
        async fn endpoint(
            bridge: SocketAddr,
            media: &str,
        ) -> (IceTransport, SocketAddr, SocketAddr) {
            let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let local = socket.local_addr().unwrap();
            let media: SocketAddr = media.parse().unwrap();
            let transport = IceTransport {
                mux: UDPMuxDefault::new(UDPMuxParams::new(Relay {
                    socket: Arc::new(socket),
                    token: "token".into(),
                    bridge,
                    media,
                })),
                ip: media.ip(),
            };
            (transport, local, media)
        }
        async fn gather(peer: &RTCPeerConnection, is_offer: bool) {
            let mut complete = peer.gathering_complete_promise().await;
            let desc = if is_offer {
                peer.create_offer(None).await.unwrap()
            } else {
                peer.create_answer(None).await.unwrap()
            };
            peer.set_local_description(desc).await.unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(5), complete.recv())
                .await
                .unwrap();
        }
        let bridge = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = bridge.local_addr().unwrap();
        let (offer_ice, offer_local, offer_virtual) = endpoint(address, "100.100.0.2:7337").await;
        let (answer_ice, answer_local, answer_virtual) =
            endpoint(address, "100.100.0.3:7337").await;
        let worker = tokio::spawn(async move {
            let mut buf = [0u8; FRAME_SIZE];
            let mut header = Vec::new();
            loop {
                let (len, source) = bridge.recv_from(&mut buf).await.unwrap();
                let Some((target, data)) = unpack(&buf[..len], "token") else {
                    continue;
                };
                let (from, to) = if source == offer_local && target == answer_virtual {
                    (offer_virtual, answer_local)
                } else if source == answer_local && target == offer_virtual {
                    (answer_virtual, offer_local)
                } else {
                    continue;
                };
                header.clear();
                use std::io::Write;
                writeln!(header, "token {from}").unwrap();
                header.extend_from_slice(data);
                bridge.send_to(&header, to).await.unwrap();
            }
        });
        let offer = APIBuilder::new()
            .with_setting_engine(offer_ice.settings())
            .build()
            .new_peer_connection(RTCConfiguration::default())
            .await
            .unwrap();
        let answer = APIBuilder::new()
            .with_setting_engine(answer_ice.settings())
            .build()
            .new_peer_connection(RTCConfiguration::default())
            .await
            .unwrap();
        let channel = offer.create_data_channel("input", None).await.unwrap();
        let (opened, ready) = tokio::sync::oneshot::channel();
        channel.on_open(Box::new(move || {
            Box::pin(async move {
                let _ = opened.send(());
            })
        }));
        gather(&offer, true).await;
        let offer_sdp = offer.local_description().await.unwrap();
        assert!(offer_sdp.sdp.contains("100.100.0.2 7337 typ host"));
        assert!(!offer_sdp.sdp.contains("127.0.0.1"));
        answer.set_remote_description(offer_sdp).await.unwrap();
        gather(&answer, false).await;
        let answer_sdp = answer.local_description().await.unwrap();
        assert!(answer_sdp.sdp.contains("100.100.0.3 7337 typ host"));
        offer.set_remote_description(answer_sdp).await.unwrap();
        let ready = tokio::time::timeout(Duration::from_secs(10), ready).await;
        offer.close().await.unwrap();
        answer.close().await.unwrap();
        offer_ice.close().await;
        answer_ice.close().await;
        worker.abort();
        ready.unwrap().unwrap();
    }
}
