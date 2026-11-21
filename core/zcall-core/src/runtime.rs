//! A UDP transport and simple poll loop that drives [`MediaSession`] on the
//! wire, so the engine can run a real 1-1 call (loopback-tested). Timers are
//! explicit poll calls rather than background threads: a thin async shell can
//! call `poll_once` on a socket timeout, or an integration test drives it
//! directly.

use crate::media::{MediaSession, Server};
use serde_json::Value;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A media session bound to a real UDP socket.
pub struct MediaDriver {
    /// The underlying media state machine.
    pub media: MediaSession,
    socket: Arc<UdpSocket>,
    /// Dedicated UDP socket for group `ShareScreenPeer` (`uid = shareScreenId`).
    share_socket: Option<Arc<UdpSocket>>,
    /// Ping period while the call is up.
    pub ping_interval: Duration,
    /// ReqForward status period while the call is up.
    pub status_interval: Duration,
    /// How long to wait for InitZRTP replies before retrying.
    pub init_wait: Duration,
    /// Delay before re-sending InitZRTP when nobody answered.
    pub init_retry: Duration,
    /// Max InitZRTP attempts.
    pub init_tries: u32,
    /// Receive buffer. A group key frame is larger than 4 KiB.
    buf: Vec<u8>,
}

impl MediaDriver {
    /// Binds an ephemeral UDP socket and wires it as the packet sender.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the socket cannot be bound.
    pub fn bind() -> Result<Self, std::io::Error> {
        let socket = Arc::new(UdpSocket::bind("0.0.0.0:0")?);
        let mut media = MediaSession::new();
        let send_socket = Arc::clone(&socket);
        media.set_send(move |packet, server, _what| {
            let _ = send_socket.send_to(packet, (server.host.as_str(), server.port));
        });
        Ok(Self {
            media,
            socket,
            share_socket: None,
            ping_interval: Duration::from_secs(6),
            status_interval: Duration::from_secs(1),
            init_wait: Duration::from_millis(600),
            init_retry: Duration::from_secs(1),
            init_tries: 3,
            buf: vec![0; 65_535],
        })
    }

    /// The bound local address.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Starts the group `ShareScreenPeer` on its own ephemeral UDP socket (or
    /// sets the 1-1 screen-share SSRC on the main socket).
    pub fn start_share(&mut self, share_id: u32) {
        if self.media.group && share_id > 0 {
            if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
                let _ = sock.set_nonblocking(true);
                let sock = Arc::new(sock);
                let send_sock = Arc::clone(&sock);
                self.media.set_share_send(move |packet, server, _what| {
                    let _ = send_sock.send_to(packet, (server.host.as_str(), server.port));
                });
                self.share_socket = Some(sock);
            }
        }
        self.media.start_share(share_id);
    }

    /// Stops screen sharing and closes any dedicated group share UDP socket.
    pub fn stop_share(&mut self) {
        self.media.stop_share();
        self.media.clear_share_send();
        self.share_socket = None;
    }

    /// Sends a raw datagram to an arbitrary address (P2P NAT hole punching).
    pub fn send_raw(&self, packet: &[u8], server: &Server) {
        let _ = self
            .socket
            .send_to(packet, (server.host.as_str(), server.port));
    }

    /// Negotiates the call parameters (servers, sessId).
    pub fn negotiate(&mut self, params: &Value) {
        self.media.on_negotiated(params);
    }

    /// Sends InitZRTP to every server.
    pub fn init_zrtp(&mut self, role: &str) {
        self.media.send_init_zrtp(role);
    }

    /// Waits for one datagram for up to `timeout`, then drains whatever else
    /// is already queued. `Duration::ZERO` does not block: a zero read timeout
    /// is rejected by the standard library and would leave the old timeout in
    /// place. At most 64 datagrams are taken per call.
    pub fn poll_once(&mut self, timeout: Duration) -> bool {
        let _ = self.socket.set_nonblocking(timeout.is_zero());
        if !timeout.is_zero() {
            let _ = self.socket.set_read_timeout(Some(timeout));
        }
        let mut any = false;
        for _ in 0..64 {
            match self.socket.recv_from(&mut self.buf) {
                Ok((n, source)) => {
                    any = true;
                    let packet = self.buf[..n].to_vec();
                    let server = Server {
                        host: source.ip().to_string(),
                        port: source.port(),
                    };
                    self.media.receive_packet(&packet, Some(&server));
                    let _ = self.socket.set_nonblocking(true);
                }
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break;
                }
                Err(_) => break,
            }
        }
        let _ = self.socket.set_nonblocking(false);
        if let Some(share_sock) = self.share_socket.as_ref() {
            for _ in 0..32 {
                match share_sock.recv_from(&mut self.buf) {
                    Ok((n, _)) => {
                        any = true;
                        let packet = self.buf[..n].to_vec();
                        self.media.receive_share_packet(&packet);
                    }
                    Err(_) => break,
                }
            }
        }
        any
    }

    /// Polls until every server answered InitZRTP, or `total` elapses.
    pub fn poll_until_ready(&mut self, total: Duration) {
        let deadline = Instant::now() + total;
        while Instant::now() < deadline {
            let answered = self.media.replies.len() + self.media.refusals.len();
            if !self.media.servers.is_empty() && answered >= self.media.servers.len() {
                break;
            }
            self.poll_once(Duration::from_millis(20));
        }
    }

    /// Enables sending and starts the call (Ping + ReqForward status).
    pub fn start(&mut self, role: &str, rtp_ser_ip: Option<&str>) {
        self.media.send_udp = true;
        self.media.start(role, rtp_ser_ip);
    }

    /// Sends one encoded Opus frame as SRTP over the relay.
    /// Returns false when the frame was not sent.
    pub fn send_audio_frame(&mut self, opus: &[u8], level: u8) -> bool {
        self.media.send_audio_frame(opus, level)
    }

    /// Whether every server has answered or refused InitZRTP.
    pub fn all_answered(&self) -> bool {
        !self.media.servers.is_empty()
            && self.media.replies.len() + self.media.refusals.len() >= self.media.servers.len()
    }

    /// Runs the full call lifecycle until ¤stop¤ is set: InitZRTP with retry,
    /// start, then the periodic Ping / ReqForward status while polling media.
    ///
    /// # Errors
    ///
    /// Returns a message when no server answered InitZRTP.
    pub fn run_call(
        &mut self,
        role: &str,
        rtp_ser_ip: Option<&str>,
        stop: &AtomicBool,
    ) -> Result<(), String> {
        for _ in 0..self.init_tries {
            self.media.send_init_zrtp(role);
            let deadline = Instant::now() + self.init_wait;
            while Instant::now() < deadline && !self.all_answered() {
                self.poll_once(Duration::from_millis(20));
            }
            if self.all_answered() {
                break;
            }
            // Only retry when nobody answered or refused at all, like the real engine.
            if !self.media.replies.is_empty() || !self.media.refusals.is_empty() {
                break;
            }
            std::thread::sleep(self.init_retry);
        }
        if self.media.replies.is_empty() {
            return Err("no server answered InitZRTP".to_owned());
        }
        self.start(role, rtp_ser_ip);

        let mut last_ping = Instant::now();
        let mut last_status = Instant::now();
        while !stop.load(Ordering::SeqCst) {
            self.poll_once(Duration::from_millis(20));
            let now = Instant::now();
            if now.duration_since(last_ping) >= self.ping_interval {
                self.media.send_ping();
                last_ping = now;
            }
            if now.duration_since(last_status) >= self.status_interval {
                self.media.send_status();
                last_status = now;
            }
        }
        self.stop();
        Ok(())
    }

    /// Ends the call on the active server.
    pub fn stop(&mut self) {
        self.media.stop();
        self.media.clear_share_send();
        self.share_socket = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtp::AUDIO_PT;
    use crate::srtp::unprotect_rtp;
    use crate::zrtp::{build_control_header, unwrap_packet, Command, MsgType, MAGIC_BYTE};

    fn reply_packet(token: u32, call_id: u32, addr: &str) -> Vec<u8> {
        let mut p = build_control_header(
            MsgType::SERVER_REPLY,
            MAGIC_BYTE,
            0,
            0,
            42,
            0,
            Command::INIT_CALL_CALLER,
            2,
        );
        p.extend_from_slice(&0_u32.to_le_bytes());
        p.extend_from_slice(&call_id.to_le_bytes());
        p.extend_from_slice(&token.to_le_bytes());
        p.extend_from_slice(&(addr.len() as u16).to_le_bytes());
        p.extend_from_slice(addr.as_bytes());
        p
    }

    #[test]
    fn driver_runs_a_loopback_call() {
        // Fake media server.
        let server = UdpSocket::bind("127.0.0.1:0").expect("server bind");
        let server_addr = server.local_addr().expect("server addr");
        let server = Arc::new(server);

        let mut driver = MediaDriver::bind().expect("driver bind");
        let driver_addr = driver.local_addr().expect("driver addr");
        let sess_id = "x".repeat(40);
        driver.negotiate(&serde_json::json!({
            "callId": 1,
            "role": "caller",
            "localUid": 42,
            "peerUid": 43,
            "sessId": sess_id,
            "servers": [{"rtpaddr": format!("127.0.0.1:{}", server_addr.port())}]
        }));
        driver.init_zrtp("caller");

        // Server receives InitZRTP.
        let mut buf = [0_u8; 4096];
        let (n, from) = server.recv_from(&mut buf).expect("server recv init");
        assert_eq!(from.port(), driver_addr.port());
        let parsed = unwrap_packet(&buf[..n]).expect("parsed init");
        assert_eq!(parsed.cmd, Some(Command::INIT_CALL_CALLER));

        // Server answers with a token.
        let reply = reply_packet(0xABCD, 1, "127.0.0.1|40000");
        server.send_to(&reply, driver_addr).expect("server reply");

        driver.poll_until_ready(Duration::from_millis(500));
        assert_eq!(driver.media.server_token, 0xABCD);
        assert_eq!(
            driver.media.active_server.as_ref().map(|s| s.host.as_str()),
            Some("127.0.0.1")
        );

        driver.start("caller", None);
        // Server receives Ping (cmd 2) and ReqForward status (cmd 32 sub 10).
        let mut saw_ping = false;
        let mut saw_status = false;
        for _ in 0..2 {
            let (n, _) = server.recv_from(&mut buf).expect("recv control");
            let p = unwrap_packet(&buf[..n]).expect("parsed control");
            if p.cmd == Some(Command::PING) {
                saw_ping = true;
            }
            if p.cmd == Some(Command::REQ_FORWARD) && p.sub_cmd == Some(0x0a) {
                saw_status = true;
            }
        }
        assert!(saw_ping);
        assert!(saw_status);

        // Send one Opus frame; the server receives MsgType 3 SRTP audio.
        driver.send_audio_frame(b"dummy-opus", 127);
        let (n, _) = server.recv_from(&mut buf).expect("recv audio");
        let parsed = unwrap_packet(&buf[..n]).expect("parsed audio");
        assert_eq!(parsed.msg_type, MsgType::AUDIO_RTP);
        assert_eq!(parsed.token, Some(0xABCD));
        let keys = driver.media.srtp_context.clone().expect("srtp context");
        let (seq, ssrc, opus) = unprotect_rtp(&parsed.payload, &keys, 0).expect("unprotect");
        assert_eq!(seq, 0);
        assert_eq!(ssrc, 42);
        assert_eq!(opus, b"dummy-opus");

        driver.stop();
        // EndCall (cmd 3) reaches the server.
        let (n, _) = server.recv_from(&mut buf).expect("recv endcall");
        let p = unwrap_packet(&buf[..n]).expect("parsed endcall");
        assert_eq!(p.cmd, Some(Command::END_CALL));
        let _ = AUDIO_PT;
    }

    #[test]
    fn driver_runs_a_timed_call() {
        let server = Arc::new(UdpSocket::bind("127.0.0.1:0").expect("server bind"));
        let server_addr = server.local_addr().expect("server addr");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_driver = Arc::clone(&stop);

        let handle = std::thread::spawn(move || {
            let mut driver = MediaDriver::bind().expect("driver bind");
            driver.ping_interval = Duration::from_millis(60);
            driver.status_interval = Duration::from_millis(60);
            driver.init_wait = Duration::from_millis(120);
            driver.init_retry = Duration::from_millis(20);
            driver.init_tries = 3;
            driver.negotiate(&serde_json::json!({
                "callId": 2,
                "role": "caller",
                "localUid": 42,
                "peerUid": 43,
                "sessId": "x".repeat(40),
                "servers": [{"rtpaddr": format!("127.0.0.1:{}", server_addr.port())}]
            }));
            let _ = driver.run_call("caller", None, &stop_for_driver);
        });

        let mut buf = [0_u8; 4096];
        let mut replied = false;
        let mut pings = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if Instant::now() >= deadline {
                break;
            }
            let _ = server.set_read_timeout(Some(Duration::from_millis(100)));
            if let Ok((n, from)) = server.recv_from(&mut buf) {
                let p = unwrap_packet(&buf[..n]).expect("parsed");
                if p.cmd == Some(Command::INIT_CALL_CALLER) && !replied {
                    replied = true;
                    server
                        .send_to(&reply_packet(0xABCD, 2, "127.0.0.1|40000"), from)
                        .expect("reply");
                } else if p.cmd == Some(Command::PING) {
                    pings += 1;
                }
                if pings >= 2 {
                    break;
                }
            }
        }
        stop.store(true, Ordering::SeqCst);
        let _ = handle.join();
        assert!(pings >= 2, "expected at least 2 pings, got {pings}");
    }
}
