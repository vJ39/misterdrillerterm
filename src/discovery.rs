//! UDPブロードキャストによる対戦相手の自動探索(#256。spec.md 12.1)。
//!
//! 同一LAN上の他ホストへHELLOを1秒間隔で流し続けながら受信も行い、候補リスト
//! (`DiscoveredPeer`)を保つ。参加リクエストのやり取り(INVITE/ACCEPT/DECLINE)と
//! ゲストからの開始要求(REQUEST_START)もこのソケットで行う。
//! 候補をどう見せるか・リクエストをどう扱うかはロビー側(`lobby.rs`)の責務で、ここは
//! 「パケットの送受信と候補リストの保守」だけを担う。

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::net::{
    DISCOVERY_PACKET_LEN, DISCOVERY_PORT, DISCOVERY_PORT_RANGE_COUNT, DISCOVERY_TIMEOUT_MS,
    DiscoveryPacket, HELLO_BROADCAST_INTERVAL_MS, PacketType,
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
    /// HELLO/BYEの送信先。実運用では`255.255.255.255:<探索範囲内の各ポート>`
    /// (#278。同一ホストで複数プロセスが別ポートを使っていても発見できるよう、
    /// 範囲内の全ポートへ送る)。
    hello_targets: Vec<SocketAddr>,
    my_id: Uuid,
    my_name: String,
    my_tcp_port: u16,
    last_hello_sent: Instant,
    peers: Vec<DiscoveredPeer>,
}

/// 環境変数`MDT_DISCOVERY_PORT`(自分がbindするポート)が明示的に指定されていれば
/// その値を返す。未設定・不正な値なら`None`(呼び出し元は範囲探索にフォールバック
/// する。#267/#278)。
fn env_bind_port_override() -> Option<u16> {
    std::env::var("MDT_DISCOVERY_PORT").ok()?.parse().ok()
}

/// 環境変数`MDT_DISCOVERY_PEER_PORT`(HELLO/招待の送信先ポート)を読み取る。未設定・
/// 不正な値なら`bind_port`と同じ値を使う(#267。通常運用と同じ「全員同じポート」)。
/// `MDT_DISCOVERY_PORT`が明示指定されている場合にのみ呼ぶ。
fn env_peer_port_override(bind_port: u16) -> u16 {
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

/// `DISCOVERY_PORT`から`DISCOVERY_PORT_RANGE_COUNT`個ぶん連続するポート範囲(#278)。
fn discovery_port_range() -> std::ops::Range<u16> {
    DISCOVERY_PORT..DISCOVERY_PORT.saturating_add(DISCOVERY_PORT_RANGE_COUNT)
}

impl Discovery {
    /// `MDT_DISCOVERY_PORT`が明示指定されていれば`0.0.0.0:<その値>`にbindし、
    /// `MDT_DISCOVERY_PEER_PORT`(未設定なら同じ値)へ送信する(#267。既存の
    /// 同一マシン動作確認手順との後方互換)。
    ///
    /// 指定が無い通常の起動では、`DISCOVERY_PORT`から連続する範囲(既定8つ)の中で
    /// 空いている最初のポートにbindし、範囲内の全ポートへHELLO/BYEをブロードキャスト
    /// する(#278。同一ホストで複数プロセスを起動しても、環境変数無しで自動的に
    /// 発見し合える)。非ブロッキング+ブロードキャスト送信可能にしてから最初のHELLOを
    /// 1回流す。
    pub fn start(my_name: String, my_tcp_port: u16) -> io::Result<Self> {
        if let Some(bind_port) = env_bind_port_override() {
            let peer_port = env_peer_port_override(bind_port);
            return Self::start_with(
                SocketAddr::from((Ipv4Addr::UNSPECIFIED, bind_port)),
                vec![SocketAddr::from((Ipv4Addr::BROADCAST, peer_port))],
                my_name,
                my_tcp_port,
            );
        }

        Self::start_in_range(
            discovery_port_range(),
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::BROADCAST,
            my_name,
            my_tcp_port,
        )
    }

    /// `ports`の中で空いている最初のポートに`bind_ip`でbindし、範囲内の全ポート
    /// (`target_ip`)へHELLO/BYEを送る(#278)。本番の`start`(`UNSPECIFIED`+
    /// `BROADCAST`)と、範囲探索そのものをテストする専用コード(ループバックの
    /// 動的なベースポート)の両方から使う。
    fn start_in_range(
        ports: impl Iterator<Item = u16> + Clone,
        bind_ip: Ipv4Addr,
        target_ip: Ipv4Addr,
        my_name: String,
        my_tcp_port: u16,
    ) -> io::Result<Self> {
        let hello_targets: Vec<SocketAddr> = ports
            .clone()
            .map(|port| SocketAddr::from((target_ip, port)))
            .collect();
        let mut last_error = None;
        for port in ports {
            match UdpSocket::bind(SocketAddr::from((bind_ip, port))) {
                Ok(socket) => {
                    return Self::start_from_socket(socket, hello_targets, my_name, my_tcp_port);
                }
                Err(err) => last_error = Some(err),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::AddrInUse, "探索用のUDPポートに空きが無い")
        }))
    }

    /// bind先とHELLOの送信先を指定して開始する。実運用の組み合わせは`start`が持ち、
    /// ここを分けているのはテストでループバックの空きポートを使うため。
    fn start_with(
        bind_addr: SocketAddr,
        hello_targets: Vec<SocketAddr>,
        my_name: String,
        my_tcp_port: u16,
    ) -> io::Result<Self> {
        let socket = UdpSocket::bind(bind_addr)?;
        Self::start_from_socket(socket, hello_targets, my_name, my_tcp_port)
    }

    /// bind済みのソケットから組み立てる(#278。範囲探索で確保したソケットを
    /// そのまま使うため`start_with`から分けている)。
    fn start_from_socket(
        socket: UdpSocket,
        hello_targets: Vec<SocketAddr>,
        my_name: String,
        my_tcp_port: u16,
    ) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        socket.set_broadcast(true)?;

        let discovery = Self {
            socket,
            hello_targets,
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
    /// 戻り値は、この呼び出しで新たに受信したINVITE/ACCEPT/DECLINE/REQUEST_STARTの
    /// うち自分宛(`target_id == my_id`)のものだけ。HELLO/BYEは候補リストの更新に使う
    /// だけで呼び出し元へは返さない。
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
                PacketType::Invite
                | PacketType::Accept
                | PacketType::Decline
                | PacketType::RequestStart => {
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

    /// ゲストからホストへの開始要求(#293)。ホストはこれを受け取ると追認なしに
    /// 対戦を開始する。
    pub fn send_request_start(&self, target: &DiscoveredPeer) -> io::Result<()> {
        self.send(
            PacketType::RequestStart,
            target.sender_id,
            peer_addr(target),
        )
    }

    /// 探索/募集からの離脱(接続確立時・ロビーを抜ける時)。相手の候補リストから
    /// 即座に消えてもらうためのもので、届かなくても`DISCOVERY_TIMEOUT_MS`後には
    /// 消えるため、送信失敗は無視する。
    pub fn send_bye(&self) {
        for &target in &self.hello_targets {
            let _ = self.send(PacketType::Bye, Uuid::nil(), target);
        }
    }

    fn send_hello(&self) {
        for &target in &self.hello_targets {
            let _ = self.send(PacketType::Hello, Uuid::nil(), target);
        }
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
        Self::start_with(loopback, vec![loopback], my_name, my_tcp_port)
    }

    /// `start`の範囲探索ロジック(#278)をループバックでテストするための入口。
    /// `base`から`count`個の範囲で空いている最初のポートにbindし、範囲内の全ポート
    /// (ループバック)へ送る。同一の`base`/`count`で複数インスタンスを起動すると、
    /// 本番の「同一ホストで複数プロセスが自動的に発見し合う」動作を再現できる。
    pub(crate) fn start_in_range_on_loopback(
        my_name: String,
        my_tcp_port: u16,
        base: u16,
        count: u16,
    ) -> io::Result<Self> {
        Self::start_in_range(
            base..base.saturating_add(count),
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::LOCALHOST,
            my_name,
            my_tcp_port,
        )
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
        self.hello_targets = vec![addr];
    }

    /// 同上のN人版(#276)。3人・4人のロビーを互いに向け合わせるため、自分以外の
    /// 全員を宛先にする。
    pub(crate) fn set_hello_targets(&mut self, addrs: Vec<SocketAddr>) {
        self.hello_targets = addrs;
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

    // -----------------------------------------------------------------------
    // 同一ホストで複数プロセスを起動しても自動的に発見し合える(#278)。
    // 固定ポート(39393-39400)は環境依存で他のテストと競合しうるため、動的に
    // 確保した空きポートを範囲の起点として使う。
    //
    // 以下のテスト名にある人数は`DISCOVERY_PORT_RANGE_COUNT`(=8、#311で4から拡張)に
    // 合わせてある。定数を変えたときは名前も合わせて直す。
    // -----------------------------------------------------------------------

    /// ループバックの空きポートを1つ確保し、その番号だけを返す(すぐ手放す)。
    /// 範囲探索の起点として使う。
    fn free_loopback_port() -> u16 {
        UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// 「起点を1つ確保して手放し、そこから続く範囲を掴み直す」手順は、他のテストが
    /// 並行して同じ手順を踏むと範囲の一部を横取りされうる(cargo testはテストを並列に
    /// 走らせるため)。この手順を使うテスト同士を直列化し、横取りを防ぐ。
    fn port_range_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn eight_processes_on_the_same_host_bind_to_distinct_ports_in_the_range() {
        // 範囲ぶんの`Discovery`を起動すると、1つ目から順に空いている最初の
        // ポートを確保していくため、全員が異なるポートになる。
        let _guard = port_range_test_lock();
        const COUNT: u16 = DISCOVERY_PORT_RANGE_COUNT;
        let base = free_loopback_port();

        let discoveries: Vec<Discovery> = (0..COUNT)
            .map(|i| {
                Discovery::start_in_range_on_loopback(format!("p{i}"), 39394 + i, base, COUNT)
                    .unwrap()
            })
            .collect();

        let ports: Vec<u16> = discoveries
            .iter()
            .map(|discovery| discovery.local_addr().port())
            .collect();
        let mut distinct = ports.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            COUNT as usize,
            "{COUNT}プロセスぶん全員が異なるポートを確保するはず(実際: {ports:?})"
        );
    }

    #[test]
    fn a_ninth_process_fails_to_start_once_the_range_is_exhausted() {
        let _guard = port_range_test_lock();
        const COUNT: u16 = DISCOVERY_PORT_RANGE_COUNT;
        let base = free_loopback_port();
        let _discoveries: Vec<Discovery> = (0..COUNT)
            .map(|i| {
                Discovery::start_in_range_on_loopback(format!("p{i}"), 39394 + i, base, COUNT)
                    .unwrap()
            })
            .collect();

        let over_capacity =
            Discovery::start_in_range_on_loopback(format!("p{COUNT}"), 39394 + COUNT, base, COUNT);

        assert!(
            over_capacity.is_err(),
            "範囲内の全ポートが使用中なら、それ以上は起動できないはず"
        );
    }

    #[test]
    fn eight_processes_on_the_same_host_discover_each_other_through_the_shared_port_range() {
        // 環境変数オーバーライド無しでも、範囲内の全ポートへ送るHELLOによって
        // 範囲ぶんのプロセスが自動的に互いを発見できる(実際のユーザー報告: 手動で
        // ポートを指定しないと2台目以降がロビーに入れなかった問題の再現・解消確認)。
        let _guard = port_range_test_lock();
        const COUNT: u16 = DISCOVERY_PORT_RANGE_COUNT;
        let base = free_loopback_port();

        let mut discoveries: Vec<Discovery> = (0..COUNT)
            .map(|i| {
                Discovery::start_in_range_on_loopback(format!("p{i}"), 39394 + i, base, COUNT)
                    .unwrap()
            })
            .collect();

        for _ in 0..200 {
            for discovery in discoveries.iter_mut() {
                discovery.resend_hello_now();
                discovery.tick();
            }
            if discoveries
                .iter()
                .all(|discovery| discovery.peers().len() == (COUNT as usize) - 1)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        for (i, discovery) in discoveries.iter().enumerate() {
            let mut names: Vec<&str> = discovery
                .peers()
                .iter()
                .map(|peer| peer.player_name.as_str())
                .collect();
            names.sort_unstable();
            let mut expected: Vec<String> = (0..COUNT)
                .filter(|&j| j != i as u16)
                .map(|j| format!("p{j}"))
                .collect();
            expected.sort_unstable();
            assert_eq!(
                names,
                expected.iter().map(String::as_str).collect::<Vec<_>>(),
                "参加者{i}は自分以外の全員を発見するはず"
            );
        }
    }
}
