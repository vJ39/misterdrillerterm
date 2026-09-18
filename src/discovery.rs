//! UDPブロードキャストによる対戦相手の自動探索(#256。spec.md 12.1)。
//!
//! 同一LAN上の他ホストへHELLOを1秒間隔で流し続けながら受信も行い、候補リスト
//! (`DiscoveredPeer`)を保つ。招待のやり取り(INVITE/ACCEPT/DECLINE)もこのソケットで行う。
//! 候補をどう見せるか・招待をどう扱うかはロビー側(`lobby.rs`)の責務で、ここは
//! 「パケットの送受信と候補リストの保守」だけを担う。

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::net::{
    DISCOVERY_PACKET_LEN, DISCOVERY_PORT, DISCOVERY_TIMEOUT_MS, DiscoveryPacket,
    HELLO_BROADCAST_INTERVAL_MS, PacketType,
};

/// HELLOを受け取って候補リストに載っている相手1件。
#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub sender_id: Uuid,
    pub player_name: String,
    pub addr: IpAddr,
    /// この相手が対戦開始後にlistenするTCPポート(HELLOで広告された値)。
    pub tcp_port: u16,
    /// 最後にこの相手からパケットを受け取った時刻。`DISCOVERY_TIMEOUT_MS`を超えると
    /// 候補リストから外す。
    last_seen: Instant,
    /// この相手のUDP送信元ポート。実運用では常に`DISCOVERY_PORT`だが、受け取った
    /// 送信元へそのまま返す形にしておくと、固定ポートを1つしか確保できない
    /// ループバックのテスト(同一プロセスで2インスタンス)でも同じ経路を検証できる。
    udp_port: u16,
}

/// 探索用のUDPソケットと候補リスト。ロビーにいる間だけ生存する。
pub struct Discovery {
    socket: UdpSocket,
    /// HELLO/BYEの送信先。実運用では`255.255.255.255:DISCOVERY_PORT`固定。
    hello_target: SocketAddr,
    my_id: Uuid,
    my_name: String,
    my_tcp_port: u16,
    last_hello_sent: Instant,
    peers: Vec<DiscoveredPeer>,
}

/// 環境変数`MDT_DISCOVERY_PORT`(自分がbindするポート)を読み取る。未設定・不正な
/// 値なら`DISCOVERY_PORT`(39393)を使う(#267)。
///
/// 同一マシンで2プロセスを起動して動作確認したい場合に使う開発用のオーバーライドで、
/// 通常のプレイでは設定不要(本番の自動探索は全ホストが39393で待ち受ける前提のまま)。
fn bind_port_override() -> u16 {
    resolve_port_override(
        std::env::var("MDT_DISCOVERY_PORT").ok().as_deref(),
        DISCOVERY_PORT,
    )
}

/// 環境変数`MDT_DISCOVERY_PEER_PORT`(HELLO/招待の送信先ポート)を読み取る。未設定・
/// 不正な値なら`bind_port`と同じ値を使う(#267。通常運用と同じ「全員同じポート」)。
fn peer_port_override(bind_port: u16) -> u16 {
    resolve_port_override(
        std::env::var("MDT_DISCOVERY_PEER_PORT").ok().as_deref(),
        bind_port,
    )
}

/// 環境変数の文字列値をポート番号として解決する。`None`・パース失敗なら`default`に
/// フォールバックする(実際の`std::env::var`呼び出しと切り離してテストできるよう、
/// 判定ロジックだけを独立させている)。
fn resolve_port_override(value: Option<&str>, default: u16) -> u16 {
    value
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

impl Discovery {
    /// 既定では`0.0.0.0:39393`にbindし、非ブロッキング+ブロードキャスト送信可能に
    /// してから最初のHELLOを1回流す。`MDT_DISCOVERY_PORT`/`MDT_DISCOVERY_PEER_PORT`が
    /// 設定されていればそちらを使う(#267。同一マシンで2プロセスを別ポートで起動し、
    /// 互いを送信先に向けることで対戦フローを実機無しに確認できる)。
    pub fn start(my_name: String, my_tcp_port: u16) -> io::Result<Self> {
        let bind_port = bind_port_override();
        let peer_port = peer_port_override(bind_port);
        Self::start_with(
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, bind_port)),
            SocketAddr::from((Ipv4Addr::BROADCAST, peer_port)),
            my_name,
            my_tcp_port,
        )
    }

    /// bind先とHELLOの送信先を指定して開始する。実運用の組み合わせは`start`が持ち、
    /// ここを分けているのはテストでループバックの空きポートを使うため。
    fn start_with(
        bind_addr: SocketAddr,
        hello_target: SocketAddr,
        my_name: String,
        my_tcp_port: u16,
    ) -> io::Result<Self> {
        let socket = UdpSocket::bind(bind_addr)?;
        socket.set_nonblocking(true)?;
        socket.set_broadcast(true)?;

        let discovery = Self {
            socket,
            hello_target,
            my_id: Uuid::new_v4(),
            my_name,
            my_tcp_port,
            last_hello_sent: Instant::now(),
            peers: Vec::new(),
        };
        // 待たずに1回流しておく(次の送信まで1秒あるため、その間相手から見えない
        // 状態が続くのを避ける)。
        discovery.send_hello();
        Ok(discovery)
    }

    pub fn peers(&self) -> &[DiscoveredPeer] {
        &self.peers
    }

    /// 1フレーム分の処理: HELLOの再送・受信キューの消化・古い候補の除去。
    ///
    /// 戻り値は、この呼び出しで新たに受信したINVITE/ACCEPT/DECLINEのうち自分宛
    /// (`target_id == my_id`)のものだけ。HELLO/BYEは候補リストの更新に使うだけで
    /// 呼び出し元へは返さない。
    pub fn tick(&mut self) -> Vec<DiscoveryPacket> {
        if self.last_hello_sent.elapsed() >= Duration::from_millis(HELLO_BROADCAST_INTERVAL_MS) {
            self.send_hello();
            self.last_hello_sent = Instant::now();
        }

        let mut for_me = Vec::new();
        let mut buffer = [0u8; DISCOVERY_PACKET_LEN];
        loop {
            // 非ブロッキングのため、受信キューが空なら`WouldBlock`で抜ける。それ以外の
            // エラーも次のtickで拾い直せばよいので、同じく打ち切る。
            let Ok((len, from)) = self.socket.recv_from(&mut buffer) else {
                break;
            };
            let Some(packet) = DiscoveryPacket::decode(&buffer[..len]) else {
                continue;
            };
            // 自分が流したブロードキャストは自分にも届く。
            if packet.sender_id == self.my_id {
                continue;
            }

            match packet.packet_type {
                PacketType::Hello => self.remember_peer(&packet, from),
                PacketType::Bye => self.forget_peer(packet.sender_id),
                PacketType::Invite | PacketType::Accept | PacketType::Decline => {
                    if packet.target_id == self.my_id {
                        for_me.push(packet);
                    }
                }
            }
        }

        let timeout = Duration::from_millis(DISCOVERY_TIMEOUT_MS);
        self.peers.retain(|peer| peer.last_seen.elapsed() < timeout);

        for_me
    }

    pub fn send_invite(&self, target: &DiscoveredPeer) -> io::Result<()> {
        self.send(PacketType::Invite, target.sender_id, peer_addr(target))
    }

    pub fn send_accept(&self, target: &DiscoveredPeer) -> io::Result<()> {
        self.send(PacketType::Accept, target.sender_id, peer_addr(target))
    }

    pub fn send_decline(&self, target: &DiscoveredPeer) -> io::Result<()> {
        self.send(PacketType::Decline, target.sender_id, peer_addr(target))
    }

    /// 探索/募集からの離脱(接続確立時・ロビーを抜ける時)。相手の候補リストから
    /// 即座に消えてもらうためのもので、届かなくても`DISCOVERY_TIMEOUT_MS`後には
    /// 消えるため、送信失敗は無視する。
    pub fn send_bye(&self) {
        let _ = self.send(PacketType::Bye, Uuid::nil(), self.hello_target);
    }

    fn send_hello(&self) {
        let _ = self.send(PacketType::Hello, Uuid::nil(), self.hello_target);
    }

    fn send(&self, packet_type: PacketType, target_id: Uuid, to: SocketAddr) -> io::Result<()> {
        let packet = DiscoveryPacket {
            packet_type,
            sender_id: self.my_id,
            target_id,
            player_name: self.my_name.clone(),
            tcp_port: self.my_tcp_port,
        };
        self.socket.send_to(&packet.encode(), to)?;
        Ok(())
    }

    /// HELLOの送り主を候補リストへ反映する。既に載っていれば表示名・アドレス・
    /// 最終受信時刻を更新する(相手が名前やポートを変えて再起動した場合に追従する)。
    fn remember_peer(&mut self, packet: &DiscoveryPacket, from: SocketAddr) {
        let now = Instant::now();
        if let Some(peer) = self
            .peers
            .iter_mut()
            .find(|peer| peer.sender_id == packet.sender_id)
        {
            peer.player_name.clone_from(&packet.player_name);
            peer.addr = from.ip();
            peer.tcp_port = packet.tcp_port;
            peer.udp_port = from.port();
            peer.last_seen = now;
            return;
        }

        self.peers.push(DiscoveredPeer {
            sender_id: packet.sender_id,
            player_name: packet.player_name.clone(),
            addr: from.ip(),
            tcp_port: packet.tcp_port,
            last_seen: now,
            udp_port: from.port(),
        });
    }

    fn forget_peer(&mut self, sender_id: Uuid) {
        self.peers.retain(|peer| peer.sender_id != sender_id);
    }
}

/// 候補へパケットを送る宛先。
fn peer_addr(peer: &DiscoveredPeer) -> SocketAddr {
    SocketAddr::new(peer.addr, peer.udp_port)
}

#[cfg(test)]
impl Discovery {
    /// テスト用。固定ポート(39393)は同一プロセスで1つしか確保できず、ブロードキャストは
    /// 環境依存で届き方が変わるため、ループバックの空きポートで開始する
    /// (設計書8節「テストでは宛先を直接指定する」)。
    pub(crate) fn start_on_loopback(my_name: String, my_tcp_port: u16) -> io::Result<Self> {
        let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        Self::start_with(loopback, loopback, my_name, my_tcp_port)
    }

    /// 自分のプレイヤーID。招待パケットの宛先(`target_id`)と突き合わせるために
    /// テストが使う(ロビー側は`Discovery`が代わりに突き合わせるため参照しない)。
    pub(crate) fn my_id(&self) -> Uuid {
        self.my_id
    }

    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.socket.local_addr().expect("bind済みのソケットのはず")
    }

    /// HELLO/BYEの送信先を差し替える。テストで2インスタンスを互いに向け合わせるため。
    pub(crate) fn set_hello_target(&mut self, addr: SocketAddr) {
        self.hello_target = addr;
    }

    /// 次の`tick`でHELLOを送らせる。1秒間隔を実時間で待たずに候補を揃えるため。
    pub(crate) fn resend_hello_now(&mut self) {
        self.last_hello_sent = Instant::now() - Duration::from_millis(HELLO_BROADCAST_INTERVAL_MS);
    }

    /// 候補を直接1件積む。実際の通信を挟まずに候補リストの見え方を確かめる用途
    /// (描画のテスト)。
    pub(crate) fn add_peer(&mut self, peer: DiscoveredPeer) {
        self.peers.push(peer);
    }
}

#[cfg(test)]
impl DiscoveredPeer {
    /// テスト用の候補。実運用ではHELLOの受信からしか作られない。
    pub(crate) fn for_test(player_name: &str, addr: IpAddr, tcp_port: u16) -> Self {
        Self {
            sender_id: Uuid::new_v4(),
            player_name: player_name.to_string(),
            addr,
            tcp_port,
            last_seen: Instant::now(),
            udp_port: DISCOVERY_PORT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // ポートのオーバーライド(#267)。実際の環境変数(グローバル状態で他のテストと
    // 競合しうる)は読まず、判定ロジックだけを純粋関数として確認する。
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_port_override_falls_back_to_the_default_when_unset_or_invalid() {
        assert_eq!(resolve_port_override(None, DISCOVERY_PORT), DISCOVERY_PORT);
        assert_eq!(
            resolve_port_override(Some(""), DISCOVERY_PORT),
            DISCOVERY_PORT
        );
        assert_eq!(
            resolve_port_override(Some("not-a-port"), DISCOVERY_PORT),
            DISCOVERY_PORT
        );
        // peer_port_overrideのフォールバック(bind_port)側の使われ方も兼ねて確認する。
        assert_eq!(resolve_port_override(None, 39400), 39400);
    }

    #[test]
    fn resolve_port_override_uses_the_given_value_when_valid() {
        assert_eq!(resolve_port_override(Some("39400"), DISCOVERY_PORT), 39400);
    }

    /// 探索側のインスタンス(ループバックの空きポートにbind)。
    fn discovery() -> Discovery {
        Discovery::start_on_loopback("me".to_string(), 39394).unwrap()
    }

    /// 相手役。`discovery`宛にユニキャストでパケットを送りつける。
    struct FakePeer {
        socket: UdpSocket,
        id: Uuid,
    }

    impl FakePeer {
        fn new() -> Self {
            Self {
                socket: UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap(),
                id: Uuid::new_v4(),
            }
        }

        fn send(&self, to: &Discovery, packet_type: PacketType, target_id: Uuid, name: &str) {
            let packet = DiscoveryPacket {
                packet_type,
                sender_id: self.id,
                target_id,
                player_name: name.to_string(),
                tcp_port: 40000,
            };
            self.socket
                .send_to(&packet.encode(), to.local_addr())
                .unwrap();
        }
    }

    /// 送ったパケットが相手のソケットに届くまでの猶予を見つつ`tick`を回す。
    /// ループバックでも配送は非同期のため、1回で届かないことがある。
    fn tick_until(discovery: &mut Discovery, mut done: impl FnMut(&Discovery) -> bool) {
        for _ in 0..200 {
            discovery.tick();
            if done(discovery) {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_hello_from_another_host_shows_up_in_the_candidate_list() {
        let mut discovery = discovery();
        let peer = FakePeer::new();

        peer.send(&discovery, PacketType::Hello, Uuid::nil(), "opponent");
        tick_until(&mut discovery, |discovery| !discovery.peers().is_empty());

        assert_eq!(discovery.peers().len(), 1);
        let found = &discovery.peers()[0];
        assert_eq!(found.sender_id, peer.id);
        assert_eq!(found.player_name, "opponent");
        assert_eq!(found.tcp_port, 40000);
        assert_eq!(found.addr, IpAddr::from(Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn repeated_hellos_from_the_same_host_update_the_entry_instead_of_adding_one() {
        let mut discovery = discovery();
        let peer = FakePeer::new();

        peer.send(&discovery, PacketType::Hello, Uuid::nil(), "before");
        tick_until(&mut discovery, |discovery| !discovery.peers().is_empty());
        peer.send(&discovery, PacketType::Hello, Uuid::nil(), "after");
        tick_until(&mut discovery, |discovery| {
            discovery.peers()[0].player_name == "after"
        });

        assert_eq!(discovery.peers().len(), 1, "同じIDの候補は増えないはず");
        assert_eq!(discovery.peers()[0].player_name, "after");
    }

    #[test]
    fn a_candidate_is_dropped_once_its_last_hello_is_older_than_the_timeout() {
        let mut discovery = discovery();
        let peer = FakePeer::new();

        peer.send(&discovery, PacketType::Hello, Uuid::nil(), "opponent");
        tick_until(&mut discovery, |discovery| !discovery.peers().is_empty());

        // 実時間で5秒待つとテストが遅くなるため、最終受信時刻を直接過去へ倒す。
        discovery.peers[0].last_seen =
            Instant::now() - Duration::from_millis(DISCOVERY_TIMEOUT_MS) - Duration::from_millis(1);
        discovery.tick();

        assert!(discovery.peers().is_empty(), "期限切れの候補は消えるはず");
    }

    #[test]
    fn a_bye_removes_the_candidate_immediately() {
        let mut discovery = discovery();
        let peer = FakePeer::new();

        peer.send(&discovery, PacketType::Hello, Uuid::nil(), "opponent");
        tick_until(&mut discovery, |discovery| !discovery.peers().is_empty());
        peer.send(&discovery, PacketType::Bye, Uuid::nil(), "opponent");
        tick_until(&mut discovery, |discovery| discovery.peers().is_empty());

        assert!(discovery.peers().is_empty());
    }

    #[test]
    fn an_invite_addressed_to_me_is_handed_to_the_caller() {
        let mut discovery = discovery();
        let peer = FakePeer::new();
        let my_id = discovery.my_id();

        peer.send(&discovery, PacketType::Invite, my_id, "opponent");
        let mut received = Vec::new();
        for _ in 0..200 {
            received = discovery.tick();
            if !received.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        assert_eq!(received.len(), 1);
        assert_eq!(received[0].packet_type, PacketType::Invite);
        assert_eq!(received[0].sender_id, peer.id);
    }

    #[test]
    fn an_invite_addressed_to_somebody_else_is_ignored() {
        let mut discovery = discovery();
        let peer = FakePeer::new();

        peer.send(&discovery, PacketType::Invite, Uuid::new_v4(), "opponent");
        // 届くだけの時間を取ってから、1件も返らないことを見る。
        let mut received = Vec::new();
        for _ in 0..50 {
            received.extend(discovery.tick());
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(
            received.is_empty(),
            "自分宛でない招待は呼び出し元へ返さない"
        );
    }

    /// `listener`に届いているHELLOの数を数える(配送待ちのため少しの間だけ粘る)。
    fn count_hellos(listener: &UdpSocket, expected_sender: Uuid) -> usize {
        let mut buffer = [0u8; DISCOVERY_PACKET_LEN];
        let mut count = 0;
        for _ in 0..50 {
            while let Ok((len, _)) = listener.recv_from(&mut buffer) {
                let Some(packet) = DiscoveryPacket::decode(&buffer[..len]) else {
                    continue;
                };
                if packet.packet_type == PacketType::Hello && packet.sender_id == expected_sender {
                    count += 1;
                }
            }
            if count > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        count
    }

    #[test]
    fn a_hello_is_resent_once_the_broadcast_interval_has_passed() {
        // 送信先を観測用のソケットへ向けて、HELLOが実際に出ているかを数える。
        let listener = UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut discovery = discovery();
        discovery.set_hello_target(listener.local_addr().unwrap());
        let my_id = discovery.my_id();

        // 開始直後のtickでは、まだ間隔に達していないので再送しない。
        discovery.tick();
        assert_eq!(
            count_hellos(&listener, my_id),
            0,
            "間隔に達する前は再送しないはず"
        );

        // 実時間で1秒待つ代わりに、前回送信時刻を間隔ぶん過去へ倒す。
        discovery.last_hello_sent =
            Instant::now() - Duration::from_millis(HELLO_BROADCAST_INTERVAL_MS);
        discovery.tick();

        assert_eq!(
            count_hellos(&listener, my_id),
            1,
            "間隔を過ぎたら1回再送するはず"
        );
    }
}
