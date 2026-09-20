//! 対戦用の状態(#252/#254/#273/#274。spec.md 12章)。
//!
//! 「全参加者の盤面を150ms固定tickでlockstep実行し、決着を確定する」状態遷移を持つ。
//! #252では通信を伴わない状態遷移だけだったが、#254で実際のTCP通信(#253)と繋ぎ、
//! 通信スレッドとのInput交換・切断検知・`Result`の交換を追加し、#255で定期的な
//! `StateHash`の照合(デシンク検出)を追加した。#256でロビー(`lobby.rs`)から
//! `Screen::Battle`へ到達する入口ができたが、2台での実プレイを自動テストでは回せない
//! ため、検証は引き続きループバックTCPを使ったユニットテストで行う。
//!
//! #273で参加者を2人固定からN人(2〜4)へ一般化した。盤面は`games: Vec<Game>`(index 0が
//! 自分)で持ち、決着は「Win/Lose/Draw」の3値から順位(`BattleOutcome::Ranked`)へ
//! 置き換えた。#274で通信経路もN人(フルメッシュ)へ広げ、自分以外の各参加者と1本ずつ
//! TCP接続を持つ(`peers`)形にした。`GameMessage`(net.rs)は変更せず、Input/Heartbeat/
//! StateHash/Result/Byeの交換は2人版のロジックをpeerごとに適用している。N人分の
//! ハンドシェイク(誰が接続を作るか)は段階C(#275)で行うため、この段階の入口は
//! 「確立済みのTCP接続がN-1本渡される」`from_peer_streams`になる。

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::constants::{
    HEARTBEAT_INTERVAL_MS, HEARTBEAT_TIMEOUT_MS, LOCKSTEP_WAIT_TIMEOUT_MS, NET_TICK_MS,
    STATE_HASH_INTERVAL_TICKS,
};
use crate::game::{Game, GameStatus, InputAction};
use crate::lockstep;
use crate::net::{self, BattleConfig, GameMessage, NetAction, NetworkEvent};

/// 1フレームの実測時間としてtickへ繰り入れる上限(ms)。通常プレイ(`tick_playing`)が
/// `Game::update`へ渡すdeltaに掛けているクランプと同じ値で、ウィンドウ非アクティブ等で
/// 大きく空いたフレームが一度に大量のtickへ化けるのを防ぐ。
const DELTA_CLAMP_MS: u64 = 250;

/// 対戦の決着(#273。spec.md 12.4)。自分視点の最終順位で表す(2人版のWin=1位/
/// Lose=2位/Draw=同着への一般化)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleOutcome {
    /// 自分の最終順位(1が1位)。同着は同じ順位になる。
    Ranked(u8),
    /// StateHashの不一致を検出して対戦を中断した(spec.md 12.3)。順位が確定しない
    /// 終わり方のため、UI表示を順位と分けられるよう区別する。
    Desync,
}

/// 対戦画面(`Screen::Battle`)が持つ状態。
///
/// `games`の各要素は通常プレイと同じ`Game`で、全員を毎tick同じ固定順序で進めることで
/// 盤面を同期する(12.3「シミュレーション自体は省略してはならない」)。
pub struct BattleState {
    /// 自分を含む全参加者の盤面。index 0が自分。フルメッシュ接続(#273設計書1節)なので、
    /// 対戦中は全参加者の視点をローカルに保持する。
    pub games: Vec<Game>,
    /// 各参加者の表示名。`games`と同じindexで対応する(index 0が自分)。
    pub player_names: Vec<String>,
    /// 実測フレーム時間を`NET_TICK_MS`(150ms)単位へ量子化するための蓄積バッファ。
    /// `lockstep::run_tick_n`は1回で150ms固定分しか進めないため、フレーム間隔が150msの
    /// 倍数からずれてもtickを取りこぼさないよう繰り越す。
    net_tick_accum: Duration,
    /// 確定した順位(1が1位)。`games`と同じindexで対応する。全員分`Some`になったら
    /// 全参加者の決着が出たことになるが、`outcome`は自分(`ranks[0]`)が確定した時点で
    /// 決まる(2人版が「自分の状態が確定したら即座にoutcome確定」だったのと同じ)。
    ranks: Vec<Option<u8>>,
    /// 次にゴール到達した参加者へ割り振る順位(1から始まり、確定するたびに人数分進む)。
    next_win_rank: u8,
    /// 次に脱落した参加者へ割り振る順位(Nから始まり、確定するたびに人数分下がる)。
    next_lose_rank: u8,
    /// 決着。`Some`になった以後はtickを進めず、自分の入力も受け付けない。
    outcome: Option<BattleOutcome>,
    /// 自分以外の各参加者との通信路(#274)。`peers[i]`は`games[i+1]`に対応する
    /// (自分が`games[0]`なので、`games[1..]`と`peers[0..]`が1対1)。`None`なら通信なしで、
    /// 他の参加者の入力は常に`None`として扱うローカル専用の動作(#252/#273のテストの前提)。
    peers: Option<Vec<PeerLink>>,
    /// 全peerへ送信済みの、現在のtickぶんの自分の入力。他の参加者の入力を待っている間に
    /// 自分の入力だけ先に確定・送信するため、tickが揃うまでここに控える
    /// (`None`なら現在のtickぶんの自分の入力はまだ確定していない)。全peerへ同じ値を
    /// 送るだけなのでpeerごとには持たない(#274設計書4節)。
    committed_local_action: Option<NetAction>,
    /// 次に処理するtick番号。全peerで共通(フルメッシュの全員が同じtickを共有するのが
    /// 前提)で、StateHashのキーや`completed_tick`の算出に使う。
    net_tick: u32,
}

/// 対戦相手1人との通信路(#274。#254の`NetworkLink`を複数保持できるよう改名し、
/// フィールドはそのまま持ち越す)。`BattleState`が対戦中ずっと保持する。
struct PeerLink {
    /// 送信用のストリーム。送信はメインループから直接行い、専用スレッドは立てない
    /// (TCPの送信バッファへ書くだけで通常は即座に返るため)。
    writer: TcpStream,
    /// 受信専用スレッドからのイベントキュー。
    event_rx: mpsc::Receiver<NetworkEvent>,
    /// Dropさせない目的だけで保持する(スレッド自体は`writer`と無関係に動く)。
    _receiver_thread: thread::JoinHandle<()>,
    /// このpeerについて次に処理するtick番号。受信した`Input`メッセージの`tick`と
    /// 突き合わせるのに使う。未切断の間は`BattleState::net_tick`と一致し、切断を
    /// 検知した時点で止まる(以後そのpeerの入力は待たないため進める意味が無い)。
    next_tick: u32,
    /// 通信の遅延で自分のtickより先に届いた相手の`Input`を、tick番号付きで
    /// 一時保持する。
    pending_remote_inputs: VecDeque<(u32, NetAction)>,
    /// 次のtickの相手の入力を待ち始めた時刻。`None`なら待機していない
    /// (前のtickまでは順調に揃っていた)。
    awaiting_since: Option<Instant>,
    /// InputまたはHeartbeatを最後に受信した時刻(`HEARTBEAT_TIMEOUT_MS`の判定に使う)。
    last_remote_activity: Instant,
    /// 最後にHeartbeatを送信した時刻。
    last_heartbeat_sent: Instant,
    /// 対戦開始時刻(ハンドシェイクの`StartCountdown`の値)。Result送信時の
    /// `time_ms`(経過時間)の算出に使う。
    start_at_unix_ms: u64,
    /// 自分のResultは送信済みか。決着直後に一度だけ送るためのフラグ。
    result_sent: bool,
    /// 相手のResultを受信済みか。受信後は自分のシミュレーション結果と照合する。
    remote_result: Option<(bool, u32)>,
    /// 相手の切断を検知済みか(Bye受信・ソケットエラー・各種タイムアウト)。
    disconnected: bool,
    /// 自分が計算し、まだ相手からの対応する`StateHash`と照合できていない
    /// (local_hash, remote_hash)。tick番号をキーに持つ。相手の到着が自分より
    /// 早い場合と遅い場合の両方があるため、双方向にキューを持つ。
    own_state_hashes: HashMap<u32, (u64, u64)>,
    /// 相手から届いたが、自分がまだそのtickに到達していない`StateHash`。
    pending_remote_state_hashes: HashMap<u32, (u64, u64)>,
}

impl PeerLink {
    /// 確立済みのTCP接続1本から通信路を組み立てる(#274)。`stream`は呼び出し元が
    /// ハンドシェイクに使ったものをそのまま渡す(内部で`try_clone`して読み書き用に分け、
    /// 読み側は受信専用スレッドへ預ける)。
    fn new(stream: TcpStream, start_at_unix_ms: u64) -> io::Result<Self> {
        // Input/Result/StateHashは1件あたり数十バイトの小さいメッセージを毎tick
        // 送り合う。Nagleアルゴリズムが有効だと、直前の送信のACKを待つ間ここが
        // バッファされ、tick間隔(NET_TICK_MS)より大きな遅延が積み重なる(#287、
        // 実機で「対戦がまだ重い」と報告された原因の一つ)。
        stream.set_nodelay(true)?;
        let reader_stream = stream.try_clone()?;
        let (tx, event_rx) = mpsc::channel();
        let receiver_thread = net::spawn_receiver_thread(reader_stream, tx);

        let now = Instant::now();
        Ok(Self {
            writer: stream,
            event_rx,
            _receiver_thread: receiver_thread,
            next_tick: 0,
            pending_remote_inputs: VecDeque::new(),
            awaiting_since: None,
            last_remote_activity: now,
            last_heartbeat_sent: now,
            start_at_unix_ms,
            result_sent: false,
            remote_result: None,
            disconnected: false,
            own_state_hashes: HashMap::new(),
            pending_remote_state_hashes: HashMap::new(),
        })
    }
}

impl BattleState {
    /// 通信なしの対戦状態を作る(#273)。`games`のindex 0が自分で、`player_names`は
    /// 同じindexで対応する表示名。`games.len()`は2〜4を想定するが、この段階では長さの
    /// 検証は行わない(呼び出し元が正しい前提。実際の人数制約は段階C/Dで扱う)。
    ///
    /// `Screen::Battle`への実際の遷移はハンドシェイク完了時(#254)・ロビーからの入口
    /// (#256)で作るため、この段階ではテストからのみ呼ばれる。
    #[allow(dead_code)]
    pub fn new(games: Vec<Game>, player_names: Vec<String>) -> Self {
        let player_count = games.len();
        Self {
            games,
            player_names,
            net_tick_accum: Duration::ZERO,
            ranks: vec![None; player_count],
            next_win_rank: 1,
            next_lose_rank: player_count as u8,
            outcome: None,
            peers: None,
            committed_local_action: None,
            net_tick: 0,
        }
    }

    /// N人分の確立済みTCP接続から通信ありの対戦状態を組み立てる(#274)。
    ///
    /// `games`/`player_names`は既にseed/configから生成済み(index 0が自分)で、`streams`は
    /// 自分以外の各参加者との接続(`games`のindex 1..と対応する順)。`start_at_unix_ms`は
    /// ハンドシェイクで合意した開始時刻(`Result`送信時の経過時間の算出に使う)。
    ///
    /// 接続の確立自体(誰が誰へ繋ぐか・configとseedの配布)は呼び出し元の責務で、N人分の
    /// ハンドシェイクは段階C(#275)で実装する。
    pub fn from_peer_streams(
        games: Vec<Game>,
        player_names: Vec<String>,
        streams: Vec<TcpStream>,
        start_at_unix_ms: u64,
    ) -> io::Result<Self> {
        debug_assert_eq!(
            games.len(),
            player_names.len(),
            "盤面と表示名は同じindexで対応するはず"
        );
        debug_assert_eq!(
            streams.len(),
            games.len() - 1,
            "接続は自分以外の参加者ぶん必要"
        );
        let player_count = games.len();
        let peers = streams
            .into_iter()
            .map(|stream| PeerLink::new(stream, start_at_unix_ms))
            .collect::<io::Result<Vec<_>>>()?;

        Ok(Self {
            games,
            player_names,
            net_tick_accum: Duration::ZERO,
            ranks: vec![None; player_count],
            next_win_rank: 1,
            next_lose_rank: player_count as u8,
            outcome: None,
            peers: Some(peers),
            committed_local_action: None,
            net_tick: 0,
        })
    }

    /// #253のハンドシェイク結果と確立済みのTCPストリームから、2人対戦の状態を
    /// 組み立てる(#254)。`stream`は呼び出し元がハンドシェイクに使ったものをそのまま渡す。
    ///
    /// 呼び出し元はロビー(`lobby.rs`)で、招待の成立後にホスト役・クライアント役の
    /// どちらの経路からもここへ合流する(#256)。`my_name`は自分の表示名で、
    /// `player_names`のindex 0に入る(ハンドシェイク結果は相手の名前しか持たないため
    /// 呼び出し元から受け取る)。
    ///
    /// N人対応のハンドシェイク(#275)ができるまで既存のロビーフローをこのシグネチャの
    /// まま使い続けられるよう、組み立ての本体は`from_peer_streams`(#274)へ委譲する。
    ///
    /// #276でロビーがN人対戦のルーム経由(`from_peer_streams`直呼び)になったため、本体
    /// からは呼ばれなくなった。2人ぶんの組み立てを確かめるテストからのみ使う。
    #[allow(dead_code)]
    pub fn from_handshake(
        handshake: net::HandshakeResult,
        stream: TcpStream,
        my_name: &str,
    ) -> io::Result<Self> {
        let games = vec![
            new_game_from_battle_config(handshake.seed, &handshake.config),
            new_game_from_battle_config(handshake.seed, &handshake.config),
        ];
        let player_names = vec![my_name.to_string(), handshake.opponent_name];

        Self::from_peer_streams(
            games,
            player_names,
            vec![stream],
            handshake.start_at_unix_ms,
        )
    }

    /// 決着(#256)。`Some`なら対戦は終わっており、画面側は結果表示へ切り替える。
    pub fn outcome(&self) -> Option<BattleOutcome> {
        self.outcome
    }

    /// 対戦から抜けることを他の参加者全員へ伝える(#256/#274)。通信なし(#252のローカル
    /// 専用)の場合は何も起きない。届かなくても相手側はHeartbeatの途絶で切断を検知する
    /// ため、送信失敗は無視する。
    pub fn notify_bye(&mut self) {
        let Some(peers) = &mut self.peers else {
            return;
        };
        for peer in peers.iter_mut() {
            if peer.disconnected {
                continue;
            }
            let _ = net::write_message(&mut peer.writer, &GameMessage::Bye);
        }
    }

    /// 全参加者の現在の`games`から、まだ確定していない参加者の順位を更新する(#273)。
    ///
    /// ゴール到達(`Cleared`)は先着順で上位から、脱落(`GameOver`)は最後まで残った順で
    /// 上位から埋まる(2人版のWin/Lose/Drawの一般化)。同一tickで複数人が同時に
    /// Cleared/GameOverになった場合は同順位にする。
    fn update_ranks(&mut self) {
        let newly_cleared: Vec<usize> = self
            .games
            .iter()
            .enumerate()
            .filter(|&(i, g)| self.ranks[i].is_none() && g.status == GameStatus::Cleared)
            .map(|(i, _)| i)
            .collect();
        let newly_over: Vec<usize> = self
            .games
            .iter()
            .enumerate()
            .filter(|&(i, g)| self.ranks[i].is_none() && g.status == GameStatus::GameOver)
            .map(|(i, _)| i)
            .collect();

        if !newly_cleared.is_empty() {
            let rank = self.next_win_rank;
            for &i in &newly_cleared {
                self.ranks[i] = Some(rank);
            }
            self.next_win_rank += newly_cleared.len() as u8;
        }
        if !newly_over.is_empty() {
            // 同時脱落は同順位にするため、その人数ぶん上の順位から割り当てる。
            let rank = self.next_lose_rank - (newly_over.len() as u8 - 1);
            for &i in &newly_over {
                self.ranks[i] = Some(rank);
            }
            self.next_lose_rank -= newly_over.len() as u8;
        }

        // 未確定者が1人だけ残ったら、その人は自動的に確定する(2人版で「相手が脱落したら
        // 自分は自動的にWin」だったのと同じ。残り1人はゴールも脱落もしていなくても
        // 順位が確定する)。
        let undecided: Vec<usize> = (0..self.games.len())
            .filter(|&i| self.ranks[i].is_none())
            .collect();
        if undecided.len() == 1 {
            self.ranks[undecided[0]] = Some(self.next_win_rank);
        }

        // 一度確定した決着は上書きしない。自分の順位が未確定なら`None`のままにする。
        if self.outcome.is_none() {
            self.outcome = self.ranks[0].map(BattleOutcome::Ranked);
        }
    }

    /// 実測の経過時間`delta`を150ms固定tickへ量子化し、溜まったぶんだけlockstepを進める。
    ///
    /// `local_action`はこのフレームで確定した自分の入力(無ければ`None`)。1tickにつき
    /// 高々1アクション(12.2)のため、1フレームで複数tick進む場合も最初のtickだけが消費し、
    /// 残りのtickは`None`で進む。決着後(`outcome`が`Some`)は何もしない。
    ///
    /// 通信あり(`peers`が`Some`)の場合は`advance_networked`へ委ねる。他の参加者の入力が
    /// 揃ったtickしか進められないため、時間の扱いがローカル専用の場合と異なる。
    pub fn advance(&mut self, delta: Duration, local_action: Option<InputAction>) {
        if self.peers.is_some() {
            self.advance_networked(delta, local_action);
            return;
        }

        // 決着後は結果表示に専念し、盤面も入力も進めない(12.4)。
        if self.outcome.is_some() {
            return;
        }

        self.net_tick_accum += delta.min(Duration::from_millis(DELTA_CLAMP_MS));

        let net_tick = Duration::from_millis(NET_TICK_MS);
        let mut local_action = local_action;
        while self.net_tick_accum >= net_tick {
            self.net_tick_accum -= net_tick;
            self.run_net_tick(local_action.take());
            if self.outcome.is_some() {
                break;
            }
        }
    }

    /// 通信ありの1フレーム(#254/#274)。他の参加者の入力が揃ったtickだけを進める。
    ///
    /// 誰かを待っている間は`net_tick_accum`へ時間を足さない(自分だけ時計が進むと
    /// lockstepの前提が壊れる)。1回の呼び出しで複数tick進む場合も、待機に入った時点で
    /// 残りのtickは次回の呼び出しへ持ち越す。
    fn advance_networked(&mut self, delta: Duration, local_action: Option<InputAction>) {
        // 受信処理だけは決着後も続ける(他の参加者の`Result`は自分の決着より後に届くため)。
        self.drain_network_events();
        if self.outcome.is_some() {
            self.reconcile_remote_results();
            return;
        }

        // 切断・タイムアウトしたpeerの順位を確定させる。残りの参加者では対戦を続けるため、
        // ここで自分の順位まで決まる(=生存者側で決着した)場合だけ`outcome`が入る。
        self.settle_disconnected_peers();
        if self.outcome.is_some() {
            return;
        }

        if self.is_awaiting_any_peer() {
            // 待機中は自分の時計を進めない(12.3)。届いていれば`drain_network_events`が
            // 既に待機を解除している。
            return;
        }
        self.send_due_heartbeats();

        self.net_tick_accum += delta.min(Duration::from_millis(DELTA_CLAMP_MS));

        let net_tick = Duration::from_millis(NET_TICK_MS);
        let mut local_action = local_action;
        while self.net_tick_accum >= net_tick {
            // 自分の入力は他の参加者を待たずに先に確定して送る。相手の入力が届いてから
            // 送る形にすると、全員が互いの`Input`を待ったまま進まなくなる。送信済みの
            // 入力はtickが揃うまで`committed_local_action`に控え、同じtickを二重に送らない。
            let my_action = match self.committed_local_action {
                Some(action) => action,
                None => {
                    let action = local_action
                        .take()
                        .and_then(Option::<NetAction>::from)
                        .unwrap_or(NetAction::None);
                    self.committed_local_action = Some(action);
                    self.send_local_input(action);
                    action
                }
            };

            let Some(all_actions) = self.take_actions_for_this_tick(my_action) else {
                // 誰かの入力がまだ無い。このtickぶんは`net_tick_accum`から引かずに
                // 次回の呼び出しへ持ち越す。
                break;
            };

            self.net_tick_accum -= net_tick;
            self.committed_local_action = None;
            let completed_tick = self.net_tick; // このtickの処理がこれから完了する
            self.net_tick += 1;
            let next_tick = self.net_tick;
            for peer in self.peers_mut() {
                if !peer.disconnected {
                    peer.next_tick = next_tick;
                }
            }
            self.run_net_tick_with_actions(&all_actions);

            // 定期的に状態ダイジェストを交換してデシンクを検出する(#255。spec.md 12.3)。
            // tick 0も対象になり、そこでの照合はハンドシェイクで合意したseed/configから
            // 同一の初期盤面が作られているかの検証を兼ねる。
            if completed_tick.is_multiple_of(STATE_HASH_INTERVAL_TICKS) {
                self.exchange_state_hashes(completed_tick);
            }

            if self.outcome.is_some() {
                self.maybe_send_results();
                self.reconcile_remote_results();
                break;
            }
        }
    }

    /// 通信ありの経路でのみ使う、全peerへの可変参照。
    fn peers_mut(&mut self) -> impl Iterator<Item = &mut PeerLink> {
        self.peers
            .as_mut()
            .expect("通信ありの経路でのみ呼ばれる")
            .iter_mut()
    }

    /// 確定した自分の入力を、未切断の全peerへ送る(#274。全員へ同じ値を送る)。
    fn send_local_input(&mut self, action: NetAction) {
        let tick = self.net_tick;
        for peer in self.peers_mut() {
            if peer.disconnected {
                continue;
            }
            let _ = net::write_message(&mut peer.writer, &GameMessage::Input { tick, action });
        }
    }

    /// このtickぶんの入力が全員分揃っていれば、`games`と同じ並びの入力列を返す(#274)。
    /// 揃っていなければ足りないpeerを待機中にして`None`を返す。
    ///
    /// 揃っていない場合は誰の入力も消費しない。先に取り出してしまうと、待機解除後に
    /// そのtickの入力が失われてlockstepが止まる。
    fn take_actions_for_this_tick(
        &mut self,
        my_action: NetAction,
    ) -> Option<Vec<Option<InputAction>>> {
        let tick = self.net_tick;
        let mut ready = true;
        for peer in self.peers_mut() {
            // 切断済みのpeerは待たない(#274設計書3節)。
            if peer.disconnected {
                continue;
            }
            if !has_remote_input_for(peer, tick) {
                peer.awaiting_since = Some(Instant::now());
                ready = false;
            }
        }
        if !ready {
            return None;
        }

        let mut all_actions = Vec::with_capacity(self.games.len());
        all_actions.push(my_action.into());
        for peer in self.peers_mut() {
            // 切断済みのpeerは`None`(このtickでは何もしない)として扱う。盤面は既に
            // `GameOver`へ倒しているため、`run_tick_n`を掛けても実害は無い。
            let action = if peer.disconnected {
                None
            } else {
                take_remote_input_for(peer, tick)
            };
            all_actions.push(action.and_then(Option::<InputAction>::from));
        }
        Some(all_actions)
    }

    /// 切断・タイムアウトを検知したpeerの順位を、まだ未確定なら確定させる(#274設計書3節)。
    ///
    /// フルメッシュの狙い(単一障害点を避ける)に合わせ、1人が抜けても全員終了にはしない。
    /// 該当peerの盤面を`GameOver`へ倒して通常の脱落と同じ経路(`update_ranks`)に乗せる
    /// ことで、切断者は最下位側から順位が埋まり、残りの参加者は対戦を続けられる。
    fn settle_disconnected_peers(&mut self) {
        let timeout = Duration::from_millis(LOCKSTEP_WAIT_TIMEOUT_MS);
        let peer_count = self.peers.as_ref().map_or(0, Vec::len);
        for i in 0..peer_count {
            let disconnected = {
                let peer = &mut self.peers.as_mut().expect("通信ありの経路でのみ呼ばれる")[i];
                // 入力待ちが上限を超えたpeerも切断扱いにする(12.4)。
                if peer
                    .awaiting_since
                    .is_some_and(|since| since.elapsed() >= timeout)
                {
                    peer.disconnected = true;
                }
                peer.disconnected
            };
            if !disconnected || self.ranks[i + 1].is_some() {
                continue;
            }
            // 実際の脱落ではないが、順位確定のためだけにこの状態を使う。
            self.games[i + 1].status = GameStatus::GameOver;
            self.update_ranks();
        }
    }

    /// 未切断のpeerの誰かの入力を待っている最中か(#274)。
    fn is_awaiting_any_peer(&self) -> bool {
        self.peers.as_ref().is_some_and(|peers| {
            peers
                .iter()
                .any(|peer| !peer.disconnected && peer.awaiting_since.is_some())
        })
    }

    /// 送信間隔を超えたpeerへHeartbeatを送る(#254の間隔ロジックをpeerごとに適用)。
    fn send_due_heartbeats(&mut self) {
        let interval = Duration::from_millis(HEARTBEAT_INTERVAL_MS);
        for peer in self.peers_mut() {
            if peer.disconnected || peer.last_heartbeat_sent.elapsed() < interval {
                continue;
            }
            let tick = peer.next_tick;
            peer.last_heartbeat_sent = Instant::now();
            let _ = net::write_message(&mut peer.writer, &GameMessage::Heartbeat { tick });
        }
    }

    /// 完了したtickの状態ダイジェストを未切断の全peerと交換・照合する(#255/#274)。
    ///
    /// peer `i`へ送る`local_hash`は自分の`games[0]`、`remote_hash`は`games[i+1]`(そのpeerに
    /// 対応するインスタンス)。相手も同じ規約で送ってくるため、2人版と同じ照合ロジックが
    /// ペアごとにそのまま使える(#274設計書1節)。
    fn exchange_state_hashes(&mut self, tick: u32) {
        let local_hash = self.games[0].state_hash();
        // `games`と`peers`の同時可変借用を避けるため、ハッシュ計算だけ先に済ませる。
        let remote_hashes: Vec<u64> = self.games[1..].iter().map(Game::state_hash).collect();

        let peers = self.peers.as_mut().expect("通信ありの経路でのみ呼ばれる");
        for (peer, remote_hash) in peers.iter_mut().zip(remote_hashes) {
            if peer.disconnected {
                continue;
            }
            peer.own_state_hashes
                .insert(tick, (local_hash, remote_hash));
            let _ = net::write_message(
                &mut peer.writer,
                &GameMessage::StateHash {
                    tick,
                    local_hash,
                    remote_hash,
                },
            );
            reconcile_state_hash(peer, &mut self.outcome, tick);
        }
    }

    /// 通信スレッドから届いたイベントを、キューが空になるまで処理する(#254)。peerごとに
    /// 独立したキューを持つため、全peerぶんを順に処理する(#274)。
    fn drain_network_events(&mut self) {
        let heartbeat_timeout = Duration::from_millis(HEARTBEAT_TIMEOUT_MS);
        let peer_count = self.peers.as_ref().map_or(0, Vec::len);
        for i in 0..peer_count {
            let peer = &mut self.peers.as_mut().expect("通信ありの経路でのみ呼ばれる")[i];

            while let Ok(event) = peer.event_rx.try_recv() {
                match event {
                    NetworkEvent::Message(GameMessage::Input { tick, action }) => {
                        peer.last_remote_activity = Instant::now();
                        peer.pending_remote_inputs.push_back((tick, action));
                    }
                    NetworkEvent::Message(GameMessage::Heartbeat { .. }) => {
                        peer.last_remote_activity = Instant::now();
                    }
                    NetworkEvent::Message(GameMessage::StateHash {
                        tick,
                        local_hash,
                        remote_hash,
                    }) => {
                        peer.last_remote_activity = Instant::now();
                        peer.pending_remote_state_hashes
                            .insert(tick, (local_hash, remote_hash));
                        // 自分が先に計算済みで相手の到着を待っていた場合は、ここで照合できる。
                        reconcile_state_hash(peer, &mut self.outcome, tick);
                    }
                    NetworkEvent::Message(GameMessage::Result {
                        reached_goal, tick, ..
                    }) => {
                        peer.remote_result = Some((reached_goal, tick));
                    }
                    NetworkEvent::Message(GameMessage::Bye) | NetworkEvent::Disconnected => {
                        peer.disconnected = true;
                    }
                    // ハンドシェイク用のメッセージは#253で消費済みのため、この段階で
                    // 届いても無視してよい。
                    NetworkEvent::Message(_) => {}
                }
            }

            // 待っていたtickの入力が届いていれば待機を解除する。
            if peer.awaiting_since.is_some() && has_remote_input_for(peer, peer.next_tick) {
                peer.awaiting_since = None;
            }

            // Input・Heartbeatのいずれも途絶えたら切断とみなす(12.4)。
            if peer.last_remote_activity.elapsed() >= heartbeat_timeout {
                peer.disconnected = true;
            }
        }
    }

    /// 決着直後に自分の`Result`を、未切断の各peerへ1回だけ送る(12.4。勝敗判定の根拠では
    /// なく相互確認用)。
    fn maybe_send_results(&mut self) {
        let reached_goal = self.games[0].status == GameStatus::Cleared;
        let Some(peers) = &mut self.peers else {
            return;
        };
        for peer in peers.iter_mut() {
            if peer.result_sent || peer.disconnected {
                continue;
            }
            peer.result_sent = true;

            let time_ms = net::unix_time_ms().saturating_sub(peer.start_at_unix_ms);
            let _ = net::write_message(
                &mut peer.writer,
                &GameMessage::Result {
                    reached_goal,
                    tick: peer.next_tick,
                    time_ms,
                },
            );
        }
    }

    /// 受信済みの各peerの`Result`を自分のシミュレーション結果と照合する(12.4)。
    ///
    /// 相手の自己申告と、自分が持つそのpeerのインスタンスの判定が食い違ったら、どちらが
    /// 正しいか判定できないため`Desync`へ上書きする(順位方式では「順位が確定しない
    /// 終わり方」が`Desync`にあたる。2人専用だった頃は同じ意図を`Draw`で表していた)。
    /// 自分がまだ決着していない段階では、単に自分のtickが相手より遅れているだけのため
    /// 照合しない。切断したpeerは順位確定のために盤面を`GameOver`へ倒しており、
    /// シミュレーション結果の照合対象にはできないため除く(#274)。
    fn reconcile_remote_results(&mut self) {
        if self.outcome.is_none() {
            return;
        }
        let peer_count = self.peers.as_ref().map_or(0, Vec::len);
        for i in 0..peer_count {
            let reached_goal_in_my_simulation = self.games[i + 1].status == GameStatus::Cleared;
            let peer = &self.peers.as_ref().expect("通信ありの経路でのみ呼ばれる")[i];
            if peer.disconnected {
                continue;
            }
            let Some((remote_reached_goal, _tick)) = peer.remote_result else {
                continue;
            };

            if remote_reached_goal != reached_goal_in_my_simulation {
                self.outcome = Some(BattleOutcome::Desync);
            }
        }
    }

    /// lockstepの1tickぶんを進め、その結果から順位・決着を更新する。通信なしの経路
    /// (#252/#273)では自分以外の参加者の入力は常に`None`になる。
    fn run_net_tick(&mut self, local_action: Option<InputAction>) {
        let mut actions = vec![None; self.games.len()];
        actions[0] = local_action;
        self.run_net_tick_with_actions(&actions);
    }

    /// `run_net_tick`の本体。通信あり(#254)の経路は、受信済みの相手の入力を含めた
    /// 全参加者ぶんの入力を直接渡す。
    fn run_net_tick_with_actions(&mut self, actions: &[Option<InputAction>]) {
        lockstep::run_tick_n(&mut self.games, actions);
        self.update_ranks();
    }
}

#[cfg(test)]
impl BattleState {
    /// 現在処理中のtick番号。テストが「目標tickまで進んだか」を判定するために使う。
    pub(crate) fn current_net_tick(&self) -> u32 {
        self.net_tick
    }

    /// 対戦の1フレームぶん`advance`を呼ぶ。実測時間を渡すのは「目標tickにまだ達して
    /// おらず、前フレームぶんの蓄積も使い切っている」ときだけにする。こうしないと
    /// 相手待ちの空回り中に時間だけが溜まり、後からまとめてtickへ化けて参加者間の
    /// tick数がずれる。
    ///
    /// ループバックで複数の`BattleState`を回すテスト(この`battle.rs`と、ルーム参加
    /// フローの`room.rs`)が共通で使う。
    pub(crate) fn pump_frame(&mut self, target_ticks: u32, action: Option<InputAction>) {
        let net_tick = Duration::from_millis(NET_TICK_MS);
        let needs_time = self.net_tick_accum < net_tick && self.net_tick < target_ticks;
        let delta = if needs_time { net_tick } else { Duration::ZERO };
        self.advance(delta, action);
    }
}

/// 指定したtickの相手の入力が届いているかだけを調べる(取り出さない。#274)。
///
/// N人版では「全員ぶん揃ってから初めて消費する」必要がある(誰か1人ぶんが未着なら
/// そのtickは持ち越すため、先に取り出してしまった他のpeerの入力が失われる)。この
/// 判定と`take_remote_input_for`の2段構えにすることで取りこぼしを防ぐ。
fn has_remote_input_for(link: &PeerLink, tick: u32) -> bool {
    link.pending_remote_inputs
        .iter()
        .any(|&(pending_tick, _)| pending_tick == tick)
}

/// `pending_remote_inputs`から指定したtickの相手の入力を取り出す。届く順序は通常
/// tick順だが、取り違えを防ぐためtick番号で突き合わせる。
fn take_remote_input_for(link: &mut PeerLink, tick: u32) -> Option<NetAction> {
    let index = link
        .pending_remote_inputs
        .iter()
        .position(|&(pending_tick, _)| pending_tick == tick)?;
    link.pending_remote_inputs
        .remove(index)
        .map(|(_, action)| action)
}

/// 指定tickについて、自分の計算値と相手からの申告値が両方揃っていれば照合する(#255)。
/// 相手の`local_hash`(相手自身の盤面)は自分の`games[i+1]`(そのpeerに対応する
/// インスタンス)のそのtick時点の値と、相手の`remote_hash`(相手から見た自分)は
/// 自分の`games[0]`のそのtick時点の値と一致するはず。不一致ならデシンクとして
/// `outcome`を`Desync`にする(spec.md 12.3)。
///
/// 送信時(自分がそのtickへ到達した時)と受信時の両方から呼ぶ。どちらが先になるかは
/// 通信の遅延次第のため、両方が揃った側の呼び出しだけが実際の照合まで進む。
fn reconcile_state_hash(link: &mut PeerLink, outcome: &mut Option<BattleOutcome>, tick: u32) {
    let Some(&(mine_local, mine_remote)) = link.own_state_hashes.get(&tick) else {
        return;
    };
    let Some(&(their_local, their_remote)) = link.pending_remote_state_hashes.get(&tick) else {
        return;
    };

    link.own_state_hashes.remove(&tick);
    link.pending_remote_state_hashes.remove(&tick);

    if outcome.is_some() {
        // 既に他の理由で決着済みなら上書きしない。デシンク検出は対戦終了前の同期ズレを
        // 捉えるためのもので、確定済みの順位を覆す根拠にはしない。
        return;
    }
    if mine_remote != their_local || mine_local != their_remote {
        *outcome = Some(BattleOutcome::Desync);
    }
}

/// `BattleConfig`とseedから、通常プレイの開始処理と同一順序でGameを1つ生成する
/// (#253。spec.md 12.2ステップ4)。ホスト・クライアントの双方がこの関数を同じ引数で
/// 呼ぶことで、4インスタンス(各ホスト2つ)の初期盤面が一致する。
///
/// 通常プレイの`start_new_game`(`main.rs`)が`Settings`から行っている反映を、`Settings`
/// ではなく`BattleConfig`から同じ並び(生成 → 速度系setter群 → 配分率の再抽選)で行う。
/// 巻き戻しストック・ブロック状態遷移ログは対戦では設定として共有しない(前者は対戦中
/// 無効、後者はシミュレーションに影響しないローカル専用。spec.md 12.5)ため、
/// `BattleConfig`にも含まれず、ここでも触らない。
pub fn new_game_from_battle_config(seed: u64, config: &BattleConfig) -> Game {
    let mut game = Game::new_with_width(
        seed,
        config.field_width as usize,
        config.depth_goal_m as usize,
    );
    game.set_block_fall_tick_ms(config.block_fall_tick_ms);
    game.set_player_fall_tick_ms(config.player_fall_tick_ms);
    game.set_shake_duration_ms(config.shake_duration_ms);
    game.set_dodge_recovery_ms(config.dodge_recovery_ms);
    game.set_move_cooldown_ms(config.move_cooldown_ms);
    game.set_bomb_spawn_rate_percent(config.bomb_spawn_rate_percent);
    game.set_bomb_fuse_ms(config.bomb_fuse_ms);
    game.set_chain_vanish_interval_ms(config.chain_vanish_interval_ms);
    game.set_attack_blocks_per_rock(config.attack_blocks_per_rock);
    game.set_attack_rocks_per_wave_max(config.attack_rocks_per_wave_max);
    // Xブロック/AIR/スター/ダイヤの配分率設定を、安全地帯明け(行2)以降の全体へ反映する。
    game.reroll_spawn_rates_from(
        2,
        config.rock_spawn_rate_percent,
        config.air_spawn_rate_percent,
        config.star_spawn_rate_percent,
        config.diamond_spawn_rate_percent,
        config.item_clear_above_rate_percent,
        config.item_unify_colors_rate_percent,
        config.item_starify_screen_rate_percent,
        config.color_count,
        config.color_cluster_rate_percent,
    );
    game
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::FIELD_WIDTH_DEFAULT;
    use crate::game::board::Cell;
    use std::collections::HashSet;
    use std::net::TcpListener;

    /// テスト用の短いコース(ゴール20m)。本番のノーマルコース(1000m)より盤面生成が軽く、
    /// ゴール到達も数tickで再現できる。
    const TEST_GOAL_M: usize = 20;

    fn net_tick() -> Duration {
        Duration::from_millis(NET_TICK_MS)
    }

    /// テスト用の`BattleConfig`。ゴールは上の短いコース(20m)に合わせる。
    fn test_battle_config() -> BattleConfig {
        BattleConfig::from_settings(&crate::settings::Settings::default(), TEST_GOAL_M)
    }

    /// 短いコースの`Game`を1つ作る。
    fn test_game(seed: u64) -> Game {
        Game::new_with_width(seed, FIELD_WIDTH_DEFAULT, TEST_GOAL_M)
    }

    /// テスト用の対戦状態。短いコースの`Game`を2つ持たせる(index 0が自分)。
    fn battle(seed_local: u64, seed_remote: u64) -> BattleState {
        battle_n(&[seed_local, seed_remote])
    }

    /// テスト用のN人対戦状態(#273)。`seeds`のindex 0が自分。表示名は`p0`,`p1`,...とする。
    fn battle_n(seeds: &[u64]) -> BattleState {
        BattleState::new(
            seeds.iter().map(|&seed| test_game(seed)).collect(),
            (0..seeds.len()).map(|i| format!("p{i}")).collect(),
        )
    }

    /// ゴール(最深行)の1つ手前に立たせ、直下を空けて「次の自由落下でゴールへ着く」状態に
    /// する。
    fn place_just_above_goal(game: &mut Game) {
        let goal_row = TEST_GOAL_M - 1;
        game.player.row = goal_row - 1;
        game.board.rows[goal_row][game.player.col] = Cell::Empty;
    }

    /// 決着がつくまで(または上限`max_ticks`まで)1tickずつ進める。
    fn advance_until_outcome(state: &mut BattleState, max_ticks: usize) {
        for _ in 0..max_ticks {
            state.advance(net_tick(), None);
            if state.outcome.is_some() {
                return;
            }
        }
    }

    #[test]
    fn reaching_the_goal_first_wins() {
        // 自分が先にゴール到達したら1位。
        let mut state = battle(1, 2);
        place_just_above_goal(&mut state.games[0]);

        advance_until_outcome(&mut state, 10);

        assert_eq!(
            state.games[0].status,
            GameStatus::Cleared,
            "前提: 自分がゴールに到達しているはず"
        );
        assert_eq!(
            state.games[1].status,
            GameStatus::Playing,
            "前提: 相手はまだゴールも脱落もしていないはず"
        );
        assert_eq!(state.outcome, Some(BattleOutcome::Ranked(1)));
        assert_eq!(
            state.ranks,
            vec![Some(1), Some(2)],
            "残り1人になった相手は自動的に2位で確定するはず"
        );
    }

    #[test]
    fn the_opponent_dropping_out_first_wins() {
        // 相手が先に脱落(酸素切れ→ライフ0)したら自分が1位。自分の側が決着要因に
        // 混ざらないよう、自分の盤面は無敵にしておく。
        let mut state = battle(3, 4);
        state.games[0].set_invincible(true);
        state.games[1].player.lives = 1;
        state.games[1].player.oxygen = 1.0;

        // 酸素切れの後、「天に召される」演出(CRUSH_ASCEND_MS=3000ms)を経てGameOverに
        // なるため、tickで十分な回数(3000msの3倍以上)を回す。
        advance_until_outcome(&mut state, 180);

        assert_eq!(
            state.games[1].status,
            GameStatus::GameOver,
            "前提: 相手が脱落しているはず"
        );
        assert_eq!(state.outcome, Some(BattleOutcome::Ranked(1)));
        assert_eq!(state.ranks, vec![Some(1), Some(2)]);
    }

    #[test]
    fn both_reaching_the_goal_on_the_same_tick_is_a_draw() {
        // 同じ盤面(同じシード)で両者を同じ位置に置くと同一tickでゴール到達する。
        // 同時ゴールは同順位(両者1位)になり、2人版のDrawに相当する。
        let mut state = battle(5, 5);
        place_just_above_goal(&mut state.games[0]);
        place_just_above_goal(&mut state.games[1]);

        advance_until_outcome(&mut state, 10);

        assert_eq!(state.games[0].status, GameStatus::Cleared);
        assert_eq!(state.games[1].status, GameStatus::Cleared);
        assert_eq!(state.ranks, vec![Some(1), Some(1)]);
        assert_eq!(state.outcome, Some(BattleOutcome::Ranked(1)));
    }

    #[test]
    fn dropping_out_first_loses() {
        // 自分が先に脱落したら最下位(2人なら2位)。通常プレイの復活ダイアログは経由しない。
        let mut state = battle(6, 7);
        state.games[1].set_invincible(true);
        state.games[0].player.lives = 1;
        state.games[0].player.oxygen = 1.0;

        advance_until_outcome(&mut state, 180);

        assert_eq!(state.games[0].status, GameStatus::GameOver);
        assert_eq!(state.outcome, Some(BattleOutcome::Ranked(2)));
        assert_eq!(state.ranks, vec![Some(2), Some(1)]);
    }

    #[test]
    fn frame_deltas_shorter_than_one_net_tick_are_carried_over() {
        // NET_TICK_MS(50ms)に満たないフレームではtickが起きず、繰り越した分と合わせて
        // NET_TICK_MSを超えた時点で1tick進む。
        let mut state = battle(8, 9);

        state.advance(Duration::from_millis(30), None);
        assert_eq!(
            state.games[0].debug_frame(),
            0,
            "NET_TICK_MSに満たないのでまだtickは起きないはず"
        );
        assert_eq!(state.net_tick_accum, Duration::from_millis(30));

        state.advance(Duration::from_millis(30), None);
        assert_eq!(
            state.games[0].debug_frame(),
            1,
            "繰り越し分と合わせてNET_TICK_MSを超えたら1tick進むはず"
        );
        assert_eq!(
            state.net_tick_accum,
            Duration::from_millis(10),
            "使い切らなかった端数は次フレームへ繰り越すはず"
        );
    }

    #[test]
    fn a_long_frame_delta_is_clamped_before_it_is_quantized() {
        // 大きく空いたフレームでも、クランプ(250ms)を超えた分はtickに化けない。
        let mut state = battle(10, 11);

        state.advance(Duration::from_secs(10), None);

        assert_eq!(
            state.games[0].debug_frame(),
            5,
            "250msにクランプされるのでNET_TICK_MS(50ms)ぶん5tickしか進まないはず"
        );
        assert_eq!(state.net_tick_accum, Duration::from_millis(0));
    }

    #[test]
    fn only_the_first_tick_of_a_frame_consumes_the_local_action() {
        // 1フレームで2tick進む場合でも、自分の入力が適用されるのは最初の1tickだけ。
        // 同じtick列を1tickずつ手で回したものと状態が一致することで確認する。
        let mut batched = battle(12, 13);
        batched.advance(Duration::from_millis(50), None); // 1tick進み0ms繰り越す
        batched.advance(Duration::from_millis(100), Some(InputAction::MoveRight)); // 2tick進む

        let mut stepwise = battle(12, 13);
        stepwise.run_net_tick(None);
        stepwise.run_net_tick(Some(InputAction::MoveRight));
        stepwise.run_net_tick(None);

        assert_eq!(batched.games[0].debug_frame(), 3, "前提: 合計3tick進むはず");
        assert_eq!(
            batched.games[0].state_hash(),
            stepwise.games[0].state_hash(),
            "2tick目以降にも入力が適用されていると状態が食い違う"
        );
    }

    #[test]
    fn no_tick_advances_after_the_outcome_is_decided() {
        // 決着後は入力を受け付けず、盤面も進めない。
        let mut state = battle(14, 15);
        place_just_above_goal(&mut state.games[0]);
        advance_until_outcome(&mut state, 10);
        assert_eq!(
            state.outcome,
            Some(BattleOutcome::Ranked(1)),
            "前提: 決着済み"
        );

        let frames_at_outcome = state.games[0].debug_frame();
        state.advance(Duration::from_millis(250), Some(InputAction::MoveRight));

        assert_eq!(
            state.games[0].debug_frame(),
            frames_at_outcome,
            "決着後はtickが進まないはず"
        );
    }

    #[test]
    fn new_game_from_battle_config_builds_the_same_board_for_the_same_arguments() {
        // 同一シード・同一設定なら、ホスト側とクライアント側で別々に生成しても
        // 初期盤面が完全に一致する(lockstepの前提。spec.md 12.2ステップ4)。
        let config = test_battle_config();

        let host_side = new_game_from_battle_config(4242, &config);
        let client_side = new_game_from_battle_config(4242, &config);

        assert_eq!(host_side.state_hash(), client_side.state_hash());
    }

    #[test]
    fn new_game_from_battle_config_applies_the_field_width_and_goal_depth() {
        let config = BattleConfig {
            field_width: 10,
            ..test_battle_config()
        };

        let game = new_game_from_battle_config(1, &config);

        assert_eq!(game.board.rows[0].len(), 10);
        assert_eq!(game.depth_goal_m(), TEST_GOAL_M);
    }

    #[test]
    fn new_game_from_battle_config_reflects_the_seed_and_the_spawn_rate_settings() {
        // 引数が効いていること(同じ値を返すだけの実装になっていないこと)を、
        // シードと配分率をそれぞれ変えて確認する。
        let config = test_battle_config();
        let base = new_game_from_battle_config(1, &config);

        assert_ne!(
            base.state_hash(),
            new_game_from_battle_config(2, &config).state_hash(),
            "シードが違えば盤面も変わるはず"
        );

        let denser_rocks = BattleConfig {
            rock_spawn_rate_percent: 300,
            ..config
        };
        assert_ne!(
            base.state_hash(),
            new_game_from_battle_config(1, &denser_rocks).state_hash(),
            "配分率の設定が盤面へ反映されているはず"
        );
    }

    // -----------------------------------------------------------------------
    // 順位判定(`update_ranks`。#273)
    // -----------------------------------------------------------------------

    /// 指定した`GameStatus`の並びで`update_ranks`を1回走らせ、確定した順位と決着を返す。
    /// 判定はstatusだけを見るため、盤面はテスト用の短いコースを使い回してよい。
    fn ranks_for(statuses: &[GameStatus]) -> (Vec<Option<u8>>, Option<BattleOutcome>) {
        let mut state = battle_n(&vec![1; statuses.len()]);
        for (game, &status) in state.games.iter_mut().zip(statuses.iter()) {
            game.status = status;
        }
        state.update_ranks();
        (state.ranks.clone(), state.outcome)
    }

    #[test]
    fn update_ranks_covers_every_combination_of_statuses_for_two_players() {
        use BattleOutcome::Ranked;
        use GameStatus::{Cleared, GameOver, Paused, Playing};

        // 2人専用だった`resolve_outcome`と同じstatusの組み合わせを、順位方式での同値
        // (Win=1位・Lose=最下位・Draw=同順位)で確認する。
        assert_eq!(ranks_for(&[Playing, Playing]), (vec![None, None], None));
        assert_eq!(ranks_for(&[Paused, Playing]), (vec![None, None], None));
        // 片方が確定すると、残った1人はゴールも脱落もしていなくても順位が決まる。
        assert_eq!(
            ranks_for(&[Cleared, Playing]),
            (vec![Some(1), Some(2)], Some(Ranked(1)))
        );
        assert_eq!(
            ranks_for(&[GameOver, Playing]),
            (vec![Some(2), Some(1)], Some(Ranked(2)))
        );
        assert_eq!(
            ranks_for(&[Playing, Cleared]),
            (vec![Some(2), Some(1)], Some(Ranked(2)))
        );
        assert_eq!(
            ranks_for(&[Playing, GameOver]),
            (vec![Some(1), Some(2)], Some(Ranked(1)))
        );
        // 同一tickで両者が同じ結末を迎えた場合は同順位(旧Draw)。
        assert_eq!(
            ranks_for(&[Cleared, Cleared]),
            (vec![Some(1), Some(1)], Some(Ranked(1)))
        );
        assert_eq!(
            ranks_for(&[GameOver, GameOver]),
            (vec![Some(1), Some(1)], Some(Ranked(1)))
        );
        // 自分がゴール・相手が脱落なら、どちらの判定でも自分が1位。
        assert_eq!(
            ranks_for(&[Cleared, GameOver]),
            (vec![Some(1), Some(2)], Some(Ranked(1)))
        );
        assert_eq!(
            ranks_for(&[GameOver, Cleared]),
            (vec![Some(2), Some(1)], Some(Ranked(2)))
        );
    }

    #[test]
    fn the_only_player_reaching_the_goal_is_ranked_first_with_three_or_four_players() {
        use BattleOutcome::Ranked;
        use GameStatus::{Cleared, Playing};

        // 1人だけがゴール到達した時点では、その1人が1位で確定し残りは未確定のまま。
        assert_eq!(
            ranks_for(&[Cleared, Playing, Playing]),
            (vec![Some(1), None, None], Some(Ranked(1)))
        );
        assert_eq!(
            ranks_for(&[Cleared, Playing, Playing, Playing]),
            (vec![Some(1), None, None, None], Some(Ranked(1)))
        );
        // ゴールしたのが自分以外なら、自分の決着はまだ出ない。
        assert_eq!(
            ranks_for(&[Playing, Cleared, Playing]),
            (vec![None, Some(1), None], None)
        );
        assert_eq!(
            ranks_for(&[Playing, Playing, Cleared, Playing]),
            (vec![None, None, Some(1), None], None)
        );
    }

    #[test]
    fn players_reaching_the_goal_on_the_same_tick_share_the_same_rank() {
        use BattleOutcome::Ranked;
        use GameStatus::{Cleared, Playing};

        // 3人で2人が同時ゴール → 2人とも1位。未確定が1人になるため残りも確定し、
        // 同着で埋まった2つぶんを飛ばした3位になる。
        assert_eq!(
            ranks_for(&[Cleared, Cleared, Playing]),
            (vec![Some(1), Some(1), Some(3)], Some(Ranked(1)))
        );
        // 4人で2人が同時ゴール → 2人とも1位、残り2人は未確定。
        assert_eq!(
            ranks_for(&[Cleared, Playing, Cleared, Playing]),
            (vec![Some(1), None, Some(1), None], Some(Ranked(1)))
        );
        // 4人全員が同時ゴール → 全員1位。
        assert_eq!(
            ranks_for(&[Cleared, Cleared, Cleared, Cleared]),
            (vec![Some(1); 4], Some(Ranked(1)))
        );
    }

    #[test]
    fn the_last_player_standing_is_ranked_first_after_everyone_else_drops_out() {
        // 4人で自分以外の3人が順番に脱落すると、最後に残った自分が自動的に1位で確定する
        // (2人版の「相手が脱落したら自動的に勝ち」の一般化)。
        let mut state = battle_n(&[1, 2, 3, 4]);

        state.games[1].status = GameStatus::GameOver;
        state.update_ranks();
        assert_eq!(
            state.ranks,
            vec![None, Some(4), None, None],
            "最初の脱落者が最下位"
        );
        assert_eq!(state.outcome, None, "自分の順位はまだ確定しないはず");

        state.games[2].status = GameStatus::GameOver;
        state.update_ranks();
        assert_eq!(state.ranks, vec![None, Some(4), Some(3), None]);
        assert_eq!(state.outcome, None);

        state.games[3].status = GameStatus::GameOver;
        state.update_ranks();
        assert_eq!(
            state.ranks,
            vec![Some(1), Some(4), Some(3), Some(2)],
            "未確定が自分1人になったら自動的に1位で確定するはず"
        );
        assert_eq!(state.outcome, Some(BattleOutcome::Ranked(1)));
    }

    #[test]
    fn a_goal_and_a_dropout_rank_from_both_ends_while_the_survivors_stay_undecided() {
        use GameStatus::{Cleared, GameOver, Playing};

        // 4人で自分がゴール・1人が脱落・残り2人がプレイ中。ゴールは上から、脱落は
        // 下から埋まり、残り2人は未確定のまま。
        assert_eq!(
            ranks_for(&[Cleared, Playing, GameOver, Playing]),
            (
                vec![Some(1), None, Some(4), None],
                Some(BattleOutcome::Ranked(1))
            )
        );
    }

    #[test]
    fn the_outcome_stays_undecided_while_i_am_one_of_the_survivors() {
        use GameStatus::{Cleared, GameOver, Playing};

        // 上と同じ状況で、自分が未確定の2人の一方である場合。他人の順位は確定しても
        // 自分の決着は出ないため、対戦は続く。
        assert_eq!(
            ranks_for(&[Playing, Cleared, GameOver, Playing]),
            (vec![None, Some(1), Some(4), None], None)
        );
    }

    #[test]
    fn a_three_player_battle_advances_every_board_and_ranks_the_goal_reacher_first() {
        // `advance`(通信なし)がN人でも全員ぶんの盤面を進め、ゴール到達者を1位にすることを
        // 実際にtickを回して確認する。
        let mut state = battle_n(&[21, 22, 23]);
        place_just_above_goal(&mut state.games[0]);

        advance_until_outcome(&mut state, 10);

        assert_eq!(state.games[0].status, GameStatus::Cleared);
        assert!(
            state.games[1..]
                .iter()
                .all(|game| game.status == GameStatus::Playing),
            "前提: 自分以外はまだゴールも脱落もしていないはず"
        );
        assert!(
            state.games[1..].iter().all(|game| game.debug_frame() > 0),
            "自分以外の盤面もtickぶん進んでいるはず"
        );
        assert_eq!(state.outcome, Some(BattleOutcome::Ranked(1)));
        assert_eq!(
            state.ranks,
            vec![Some(1), None, None],
            "残り2人は未確定のままのはず"
        );
    }

    // -----------------------------------------------------------------------
    // 通信あり(#254)。ループバックTCPで#253のハンドシェイクを実際に行ってから、
    // 2つの`BattleState`をlockstepで進める。
    // -----------------------------------------------------------------------

    /// テストのポンプ回数の上限。ループバックの配送待ちで何周か空回りするため実際の
    /// tick数より多めに取る。ここまで回して条件が揃わなければ実装の不具合とみなす。
    const MAX_PUMPS: usize = 500;

    /// 1周ごとに挟む待ち時間。受信スレッドがメッセージを届ける隙を作るためのもので、
    /// これが無いとポンプの空回りだけで上限に達し、相手の入力が届く前に打ち切られる。
    fn pump_interval() {
        thread::sleep(Duration::from_millis(1));
    }

    /// 通信ありの状態が持つ`PeerLink`のうち`index`番目(`games[index + 1]`に対応)を
    /// 取り出す。
    fn link_at(state: &BattleState, index: usize) -> &PeerLink {
        &state.peers.as_ref().expect("通信ありの対戦状態のはず")[index]
    }

    /// 2人対戦で唯一の`PeerLink`を取り出す(#254のテスト群用)。
    fn link(state: &BattleState) -> &PeerLink {
        link_at(state, 0)
    }

    /// ループバックTCPで#253のハンドシェイクを実行し、ホスト側・クライアント側の
    /// `BattleState`と、両者が合意した内容(参照実装の組み立てに使う)を返す。
    fn connected_pair() -> (BattleState, BattleState, net::HandshakeResult) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let config = test_battle_config();

        let host = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let result = net::run_host_handshake(&mut stream, "host", config).unwrap();
            (result, stream)
        });

        let mut client_stream = TcpStream::connect(addr).unwrap();
        let client_result = net::run_client_handshake(&mut client_stream, "client").unwrap();
        let (host_result, host_stream) = host.join().unwrap();
        let agreed = host_result.clone();

        (
            BattleState::from_handshake(host_result, host_stream, "host").unwrap(),
            BattleState::from_handshake(client_result, client_stream, "client").unwrap(),
            agreed,
        )
    }

    /// ホスト側だけ`BattleState`を作り、相手側は生のTCPストリームのままにする。
    /// 偽のメッセージを送りつけるテスト用。
    fn battle_with_raw_peer() -> (BattleState, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let config = test_battle_config();

        let host = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let result = net::run_host_handshake(&mut stream, "host", config).unwrap();
            (result, stream)
        });

        let mut peer = TcpStream::connect(addr).unwrap();
        net::run_client_handshake(&mut peer, "peer").unwrap();
        let (host_result, host_stream) = host.join().unwrap();

        (
            BattleState::from_handshake(host_result, host_stream, "host").unwrap(),
            peer,
        )
    }

    /// 対戦の1フレームぶん進める(実体は`BattleState::pump_frame`。`room.rs`のテストと
    /// 共通化したもの)。
    fn pump(state: &mut BattleState, target_ticks: u32, action: Option<InputAction>) {
        state.pump_frame(target_ticks, action);
    }

    /// `tick`番目に入力する予定のアクション(尽きたら何もしない)。
    fn action_for(actions: &[Option<InputAction>], tick: u32) -> Option<InputAction> {
        actions.get(tick as usize).copied().flatten()
    }

    #[test]
    fn two_hosts_connected_over_tcp_advance_in_lockstep() {
        // 双方が異なる入力列を送り合っても、両視点のクロスチェック(A.local⟷B.remote)が
        // 一致し、かつ#251のローカルハーネス(`lockstep::run_tick`)と同じ結果になる。
        const TICKS: u32 = 12;
        let host_actions: Vec<Option<InputAction>> = (0..TICKS)
            .map(|i| match i % 4 {
                0 => Some(InputAction::MoveRight),
                1 => Some(InputAction::Drill),
                2 => None,
                _ => Some(InputAction::FaceDown),
            })
            .collect();
        let client_actions: Vec<Option<InputAction>> = (0..TICKS)
            .map(|i| match i % 3 {
                0 => Some(InputAction::MoveLeft),
                1 => Some(InputAction::Drill),
                _ => None,
            })
            .collect();

        let (mut host, mut client, agreed) = connected_pair();
        for _ in 0..MAX_PUMPS {
            let host_action = action_for(&host_actions, link(&host).next_tick);
            pump(&mut host, TICKS, host_action);
            let client_action = action_for(&client_actions, link(&client).next_tick);
            pump(&mut client, TICKS, client_action);
            if link(&host).next_tick >= TICKS && link(&client).next_tick >= TICKS {
                break;
            }
            pump_interval();
        }

        assert_eq!(link(&host).next_tick, TICKS, "ホストが目標tickまで進むはず");
        assert_eq!(
            link(&client).next_tick,
            TICKS,
            "クライアントが目標tickまで進むはず"
        );
        assert_eq!(host.outcome, None, "前提: この長さでは決着しないはず");
        assert_eq!(client.outcome, None);

        assert_eq!(
            host.games[0].state_hash(),
            client.games[1].state_hash(),
            "ホストの自分盤面とクライアントの相手盤面が一致しない"
        );
        assert_eq!(
            host.games[1].state_hash(),
            client.games[0].state_hash(),
            "ホストの相手盤面とクライアントの自分盤面が一致しない"
        );

        let mut reference_host = new_game_from_battle_config(agreed.seed, &agreed.config);
        let mut reference_client = new_game_from_battle_config(agreed.seed, &agreed.config);
        for tick in 0..TICKS as usize {
            lockstep::run_tick(
                &mut reference_host,
                &mut reference_client,
                host_actions[tick],
                client_actions[tick],
            );
        }
        assert_eq!(
            host.games[0].state_hash(),
            reference_host.state_hash(),
            "通信を挟んでもローカルハーネスと同じ結果になるはず"
        );
        assert_eq!(host.games[1].state_hash(), reference_client.state_hash());
    }

    #[test]
    fn an_opponent_that_stops_sending_input_times_out_into_a_win_by_default() {
        // 相手が`advance`を呼ばなくなる(入力を送らなくなる)と、500ms待って不戦勝になる。
        // `client`は束縛したままにして接続自体は生かす(切断検知ではなくtick待ちの
        // タイムアウトで決着することを見るため)。
        let (mut host, _client, _) = connected_pair();

        let started = Instant::now();
        let deadline = Duration::from_millis(LOCKSTEP_WAIT_TIMEOUT_MS * 8);
        while host.outcome.is_none() && started.elapsed() < deadline {
            host.advance(net_tick(), None);
            thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(host.outcome, Some(BattleOutcome::Ranked(1)));
        assert!(
            started.elapsed() >= Duration::from_millis(LOCKSTEP_WAIT_TIMEOUT_MS),
            "待機上限に達する前に不戦勝にはしないはず"
        );
        assert!(link(&host).disconnected, "切断扱いになるはず");
        assert_eq!(link(&host).next_tick, 0, "1tickも進めずに終わるはず");
    }

    #[test]
    fn receiving_bye_from_the_opponent_ends_the_battle_as_a_win_by_default() {
        let (mut host, mut peer) = battle_with_raw_peer();
        net::write_message(&mut peer, &GameMessage::Bye).unwrap();

        // `delta`を0にして回すと`net_tick_accum`が溜まらず相手待ちにも入らないため、
        // tick待ちのタイムアウトではなくBye受信だけで決着することを確認できる。
        for _ in 0..MAX_PUMPS {
            host.advance(Duration::ZERO, None);
            if host.outcome.is_some() {
                break;
            }
            pump_interval();
        }

        assert_eq!(host.outcome, Some(BattleOutcome::Ranked(1)));
        assert!(link(&host).disconnected);
        assert!(
            link(&host).awaiting_since.is_none(),
            "tick待ちには入っていないはず(Bye受信での決着)"
        );
    }

    #[test]
    fn a_heartbeat_is_sent_while_neither_side_ticks() {
        // どちらのtickも動いていない間は、生存を示すのがHeartbeatだけになる(12.4)。
        let (mut host, mut peer) = battle_with_raw_peer();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(HEARTBEAT_INTERVAL_MS + 200) {
            // `delta`が0ならtickは進まないため、Inputは1件も送られない。
            host.advance(Duration::ZERO, None);
            thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(
            net::read_message(&mut peer).unwrap(),
            GameMessage::Heartbeat { tick: 0 }
        );
    }

    #[test]
    fn silence_longer_than_the_heartbeat_timeout_counts_as_a_disconnect() {
        // 相手役はInputもHeartbeatも送らない。tickを進めない(delta=0)ため、tick待ちの
        // タイムアウトではなくHeartbeatの途絶だけで切断と判定される。
        let (mut host, _peer) = battle_with_raw_peer();

        let started = Instant::now();
        let deadline = Duration::from_millis(HEARTBEAT_TIMEOUT_MS * 2);
        while host.outcome.is_none() && started.elapsed() < deadline {
            host.advance(Duration::ZERO, None);
            thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(host.outcome, Some(BattleOutcome::Ranked(1)));
        assert!(
            started.elapsed() >= Duration::from_millis(HEARTBEAT_TIMEOUT_MS),
            "途絶の上限に達する前に切断扱いにはしないはず"
        );
        assert!(
            link(&host).awaiting_since.is_none(),
            "tick待ちのタイムアウトではないはず"
        );
    }

    #[test]
    fn a_consistent_result_exchange_leaves_both_outcomes_untouched() {
        // 「ホスト側のプレイヤーがゴール直前にいる」状態を両ホストで同一に作る
        // (ホストから見た自分の盤面=クライアントから見た相手の盤面)。
        const TICKS: u32 = 20;
        let (mut host, mut client, _) = connected_pair();
        place_just_above_goal(&mut host.games[0]);
        place_just_above_goal(&mut client.games[1]);

        for _ in 0..MAX_PUMPS {
            pump(&mut host, TICKS, None);
            pump(&mut client, TICKS, None);
            if link(&host).remote_result.is_some() && link(&client).remote_result.is_some() {
                break;
            }
            pump_interval();
        }

        assert_eq!(
            host.games[0].status,
            GameStatus::Cleared,
            "前提: ホスト側がゴールに到達しているはず"
        );
        assert_eq!(host.outcome, Some(BattleOutcome::Ranked(1)));
        assert_eq!(client.outcome, Some(BattleOutcome::Ranked(2)));
        assert_eq!(
            link(&host).remote_result.map(|(reached, _)| reached),
            Some(false),
            "相手は「自分はゴールしていない」と申告するはず"
        );
        assert_eq!(
            link(&client).remote_result.map(|(reached, _)| reached),
            Some(true),
            "相手は「自分がゴールした」と申告するはず"
        );

        // 申告と自分のシミュレーションが一致しているため、決着は上書きされない。
        for _ in 0..10 {
            host.advance(Duration::ZERO, None);
            client.advance(Duration::ZERO, None);
        }
        assert_eq!(host.outcome, Some(BattleOutcome::Ranked(1)));
        assert_eq!(client.outcome, Some(BattleOutcome::Ranked(2)));
    }

    #[test]
    fn a_contradictory_result_from_the_opponent_overwrites_the_outcome_with_a_desync() {
        // 相手の申告と自分のシミュレーションが食い違ったら、順位を確定できない終わり方
        // (`Desync`)へ上書きする(12.4)。
        const TICKS: u32 = 16;
        let (mut host, mut peer) = battle_with_raw_peer();
        place_just_above_goal(&mut host.games[0]);

        // 相手役はtickを進めるための入力だけ送る(自分は何もしない)。
        for tick in 0..TICKS {
            net::write_message(
                &mut peer,
                &GameMessage::Input {
                    tick,
                    action: NetAction::None,
                },
            )
            .unwrap();
        }
        for _ in 0..MAX_PUMPS {
            pump(&mut host, TICKS, None);
            if host.outcome.is_some() {
                break;
            }
            pump_interval();
        }

        assert_eq!(
            host.games[0].status,
            GameStatus::Cleared,
            "前提: 自分のゴール到達で決着しているはず"
        );
        assert_eq!(host.outcome, Some(BattleOutcome::Ranked(1)));

        // ホストの持つ相手インスタンスはまだプレイ中なのに、相手は「自分がゴールした」と
        // 申告してくる。
        net::write_message(
            &mut peer,
            &GameMessage::Result {
                reached_goal: true,
                tick: link(&host).next_tick,
                time_ms: 0,
            },
        )
        .unwrap();
        for _ in 0..MAX_PUMPS {
            host.advance(Duration::ZERO, None);
            if host.outcome == Some(BattleOutcome::Desync) {
                break;
            }
            pump_interval();
        }

        assert_eq!(
            host.games[1].status,
            GameStatus::Playing,
            "前提: 自分のシミュレーション上、相手はゴールしていない"
        );
        assert_eq!(host.outcome, Some(BattleOutcome::Desync));
    }

    // -----------------------------------------------------------------------
    // デシンク検出(#255。spec.md 12.3)。
    // -----------------------------------------------------------------------

    /// 生ストリーム側で、最初の`StateHash`が届くまでメッセージを読み進める。同じtickの
    /// `Input`が先に届くため、種別で選り分ける必要がある。
    fn read_state_hash(peer: &mut TcpStream) -> GameMessage {
        loop {
            let msg = net::read_message(peer).expect("StateHashが届くはず");
            if matches!(msg, GameMessage::StateHash { .. }) {
                return msg;
            }
        }
    }

    #[test]
    fn state_hashes_are_exchanged_and_reconciled_across_several_intervals() {
        // `STATE_HASH_INTERVAL_TICKS`を跨いで進めると、tick 0とtick 20のStateHashが
        // 双方向に送られる。実際に同期しているため照合はすべて通り、控えていた値は
        // 両側のキューから消える。
        const TICKS: u32 = STATE_HASH_INTERVAL_TICKS + 5;
        let (mut host, mut client, _) = connected_pair();

        for _ in 0..MAX_PUMPS {
            pump(&mut host, TICKS, None);
            pump(&mut client, TICKS, None);
            if link(&host).next_tick >= TICKS && link(&client).next_tick >= TICKS {
                break;
            }
            pump_interval();
        }

        assert_eq!(link(&host).next_tick, TICKS, "ホストが目標tickまで進むはず");
        assert_eq!(
            link(&client).next_tick,
            TICKS,
            "クライアントが目標tickまで進むはず"
        );

        // 最後(tick 20ぶん)のStateHashが相手側で処理されるまで、受信だけ回す。
        for _ in 0..10 {
            host.advance(Duration::ZERO, None);
            client.advance(Duration::ZERO, None);
            pump_interval();
        }

        assert_eq!(
            host.outcome, None,
            "同期が取れていればデシンクにはならないはず"
        );
        assert_eq!(client.outcome, None);
        for (side, state) in [("ホスト", &host), ("クライアント", &client)] {
            assert!(
                link(state).own_state_hashes.is_empty(),
                "{side}: 自分の計算値がすべて照合済みのはず"
            );
            assert!(
                link(state).pending_remote_state_hashes.is_empty(),
                "{side}: 相手からの申告値がすべて照合済みのはず"
            );
        }
    }

    #[test]
    fn a_mismatching_state_hash_from_the_opponent_ends_the_battle_as_a_desync() {
        // 相手の申告と自分の計算が食い違ったら、どちらの状態が正しいか判定できないため
        // デシンクとして中断する(12.3)。
        let (mut host, mut peer) = battle_with_raw_peer();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

        // 相手役はtick 0を完了させるための入力だけ送る。
        net::write_message(
            &mut peer,
            &GameMessage::Input {
                tick: 0,
                action: NetAction::None,
            },
        )
        .unwrap();
        for _ in 0..MAX_PUMPS {
            pump(&mut host, 1, None);
            if link(&host).next_tick >= 1 {
                break;
            }
            pump_interval();
        }
        assert_eq!(link(&host).next_tick, 1, "前提: tick 0が完了しているはず");

        // ホストはtick 0の完了時に自分の2インスタンスのダイジェストを送ってくる。
        assert_eq!(
            read_state_hash(&mut peer),
            GameMessage::StateHash {
                tick: 0,
                local_hash: host.games[0].state_hash(),
                remote_hash: host.games[1].state_hash(),
            },
            "自分の盤面・相手の盤面のダイジェストをそのまま申告するはず"
        );

        // 相手役は「自分の盤面」として、ホストのgames[1]とは違う値を申告する。
        let (mine_local, mine_remote) = link(&host).own_state_hashes[&0];
        net::write_message(
            &mut peer,
            &GameMessage::StateHash {
                tick: 0,
                local_hash: mine_remote ^ 1,
                remote_hash: mine_local,
            },
        )
        .unwrap();
        for _ in 0..MAX_PUMPS {
            host.advance(Duration::ZERO, None);
            if host.outcome.is_some() {
                break;
            }
            pump_interval();
        }

        assert_eq!(host.outcome, Some(BattleOutcome::Desync));
        assert!(
            link(&host).own_state_hashes.is_empty(),
            "照合の済んだtickは控えから消えるはず"
        );
        assert!(link(&host).pending_remote_state_hashes.is_empty());
    }

    #[test]
    fn a_mismatching_state_hash_does_not_overwrite_an_outcome_that_is_already_decided() {
        // デシンク検出は対戦終了前の同期ズレを捉えるためのもので、確定済みの決着は覆さない。
        const TICKS: u32 = 16;
        let (mut host, mut peer) = battle_with_raw_peer();
        place_just_above_goal(&mut host.games[0]);

        // 相手役はtickを進めるための入力だけ送る(自分は何もしない)。
        for tick in 0..TICKS {
            net::write_message(
                &mut peer,
                &GameMessage::Input {
                    tick,
                    action: NetAction::None,
                },
            )
            .unwrap();
        }
        for _ in 0..MAX_PUMPS {
            pump(&mut host, TICKS, None);
            if host.outcome.is_some() {
                break;
            }
            pump_interval();
        }
        assert_eq!(
            host.outcome,
            Some(BattleOutcome::Ranked(1)),
            "前提: 自分のゴール到達で決着しているはず"
        );

        // tick 0のぶんは相手の申告が来ないまま決着したため、控えに残っている。
        let (mine_local, mine_remote) = link(&host).own_state_hashes[&0];
        net::write_message(
            &mut peer,
            &GameMessage::StateHash {
                tick: 0,
                local_hash: mine_remote ^ 1,
                remote_hash: mine_local,
            },
        )
        .unwrap();
        for _ in 0..MAX_PUMPS {
            host.advance(Duration::ZERO, None);
            if link(&host).own_state_hashes.is_empty() {
                break;
            }
            pump_interval();
        }

        assert!(
            link(&host).own_state_hashes.is_empty(),
            "前提: 照合自体は行われるはず"
        );
        assert_eq!(
            host.outcome,
            Some(BattleOutcome::Ranked(1)),
            "決着済みの結果はデシンク検出で上書きされないはず"
        );
    }

    // -----------------------------------------------------------------------
    // フルメッシュ通信(#274)。N人がそれぞれ他の全員と1本ずつTCP接続を持つ構成を
    // ループバックで組み、`from_peer_streams`で各参加者の`BattleState`を作る。
    // ハンドシェイク(#275)は範囲外のため、接続はテスト側で直接用意する。
    // -----------------------------------------------------------------------

    /// ループバックで繋がったTCPソケットの組を作る。
    fn loopback_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let outgoing = TcpStream::connect(addr).unwrap();
        let (incoming, _) = listener.accept().unwrap();
        (incoming, outgoing)
    }

    /// 参加者`h`が持つ`games`/`player_names`の並び順(index 0が自分、残りは参加者番号順)。
    /// `BattleState`の「index 0が自分」という規約に合わせたもの。
    fn participant_order(n: usize, h: usize) -> Vec<usize> {
        let mut order = vec![h];
        order.extend((0..n).filter(|&p| p != h));
        order
    }

    /// 参加者`h`の視点で、参加者`p`のインスタンスが`games`の何番目にあるか。
    fn games_index_of(n: usize, h: usize, p: usize) -> usize {
        participant_order(n, h)
            .iter()
            .position(|&q| q == p)
            .expect("並び順には全参加者が含まれるはず")
    }

    /// `n`人ぶんのフルメッシュ(C(n,2)本の接続)を張り、各参加者の`BattleState`を返す。
    /// 戻り値のindexは参加者番号。
    fn connected_mesh(n: usize, seed: u64) -> Vec<BattleState> {
        let config = test_battle_config();
        // `sockets[a][b]`=参加者aから参加者bへ向かう接続。
        let mut sockets: Vec<Vec<Option<TcpStream>>> =
            (0..n).map(|_| (0..n).map(|_| None).collect()).collect();
        for (a, b) in (0..n).flat_map(|a| ((a + 1)..n).map(move |b| (a, b))) {
            let (to_b, to_a) = loopback_pair();
            sockets[a][b] = Some(to_b);
            sockets[b][a] = Some(to_a);
        }

        // 開始時刻はハンドシェイク(#275)で合意する値の代わり。
        let start_at_unix_ms = net::unix_time_ms();
        let mut states = Vec::with_capacity(n);
        for (h, mut row) in sockets.into_iter().enumerate() {
            let order = participant_order(n, h);
            let games: Vec<Game> = order
                .iter()
                .map(|_| new_game_from_battle_config(seed, &config))
                .collect();
            let player_names: Vec<String> = order.iter().map(|&p| format!("p{p}")).collect();
            let streams: Vec<TcpStream> = order[1..]
                .iter()
                .map(|&p| row[p].take().expect("各ペアに1本ずつ用意している"))
                .collect();
            states.push(
                BattleState::from_peer_streams(games, player_names, streams, start_at_unix_ms)
                    .unwrap(),
            );
        }
        states
    }

    /// 参加者0だけ`BattleState`を作り、他の参加者は生のTCPストリームのままにする。
    /// 偽のメッセージを送りつけるテスト用(`battle_with_raw_peer`のN人版)。
    /// 戻り値の`peers[i]`は`games[i+1]`に対応する。
    fn battle_with_raw_peers(n: usize, seed: u64) -> (BattleState, Vec<TcpStream>) {
        let config = test_battle_config();
        let mut host_streams = Vec::with_capacity(n - 1);
        let mut raw_peers = Vec::with_capacity(n - 1);
        for _ in 1..n {
            let (mine, theirs) = loopback_pair();
            host_streams.push(mine);
            raw_peers.push(theirs);
        }

        let games: Vec<Game> = (0..n)
            .map(|_| new_game_from_battle_config(seed, &config))
            .collect();
        let player_names: Vec<String> = (0..n).map(|p| format!("p{p}")).collect();
        let state =
            BattleState::from_peer_streams(games, player_names, host_streams, net::unix_time_ms())
                .unwrap();
        (state, raw_peers)
    }

    /// 全参加者の視点で、同じ参加者のインスタンスが一致していることを確認する
    /// (2人版`two_hosts_connected_over_tcp_advance_in_lockstep`のクロスチェックの一般化)。
    /// `states`のindexは参加者番号。
    fn assert_mesh_views_agree(states: &[BattleState], label: &str) {
        let n = states.len();
        for p in 0..n {
            let expected = states[0].games[games_index_of(n, 0, p)].state_hash();
            for (h, state) in states.iter().enumerate().skip(1) {
                assert_eq!(
                    expected,
                    state.games[games_index_of(n, h, p)].state_hash(),
                    "{label}: 参加者{p}のインスタンスが参加者0と参加者{h}で一致しない"
                );
            }
        }
    }

    /// `n`人のフルメッシュで、全員が異なる入力列を送り合っても盤面が一致し続け、かつ
    /// ローカルハーネス(`lockstep::run_tick_n`)と同じ結果になることを確認する。
    fn assert_full_mesh_lockstep(n: usize, seed: u64, ticks: u32) {
        // 参加者ごとに周期をずらし、盤面が互いに違うものになるようにする。
        let actions: Vec<Vec<Option<InputAction>>> = (0..n)
            .map(|p| {
                (0..ticks as usize)
                    .map(|t| match (t + p) % 5 {
                        0 => Some(InputAction::MoveRight),
                        1 => Some(InputAction::Drill),
                        2 => None,
                        3 => Some(InputAction::FaceDown),
                        _ => Some(InputAction::MoveLeft),
                    })
                    .collect()
            })
            .collect();

        let mut states = connected_mesh(n, seed);
        for _ in 0..MAX_PUMPS {
            for (p, state) in states.iter_mut().enumerate() {
                let action = action_for(&actions[p], state.net_tick);
                pump(state, ticks, action);
            }
            if states.iter().all(|state| state.net_tick >= ticks) {
                break;
            }
            pump_interval();
        }

        for (p, state) in states.iter().enumerate() {
            assert_eq!(state.net_tick, ticks, "参加者{p}が目標tickまで進むはず");
            assert_eq!(state.outcome, None, "参加者{p}: この範囲では決着しないはず");
            assert!(
                state
                    .peers
                    .as_ref()
                    .is_some_and(|peers| peers.len() == n - 1),
                "参加者{p}: 自分以外の全員と接続を持つはず"
            );
        }
        assert_mesh_views_agree(&states, &format!("tick{ticks}後"));

        // #273のローカルハーネスと同じ結果になることも確認する。
        let config = test_battle_config();
        let mut reference: Vec<Game> = (0..n)
            .map(|_| new_game_from_battle_config(seed, &config))
            .collect();
        // 参加者ごとの入力列を、tickごと(`run_tick_n`が取る並び)へ組み替える。
        let by_tick: Vec<Vec<Option<InputAction>>> = (0..ticks as usize)
            .map(|t| {
                actions
                    .iter()
                    .map(|per_participant| per_participant[t])
                    .collect()
            })
            .collect();
        for tick_actions in &by_tick {
            lockstep::run_tick_n(&mut reference, tick_actions);
        }
        for (h, state) in states.iter().enumerate() {
            for (p, expected) in reference.iter().enumerate() {
                assert_eq!(
                    state.games[games_index_of(n, h, p)].state_hash(),
                    expected.state_hash(),
                    "参加者{h}の視点の参加者{p}がローカルハーネスと一致しない"
                );
            }
        }

        // 一致比較が自明に通る状況(盤面が初期状態のまま・全員同じ盤面)になっていない
        // ことの裏取り。入力列は参加者ごとに違うが、移動が打ち消し合って同じ盤面に
        // 行き着く組み合わせもあるため、全員が互いに異なることまでは求めない。
        let initial = new_game_from_battle_config(seed, &config).state_hash();
        let distinct: HashSet<u64> = reference.iter().map(Game::state_hash).collect();
        assert!(
            !distinct.contains(&initial),
            "どの参加者の盤面も初期状態からは進んでいるはず"
        );
        assert!(
            distinct.len() >= 2,
            "入力列が違うため、少なくとも一部の参加者の盤面は互いに異なるはず"
        );
    }

    #[test]
    fn three_participants_in_a_full_mesh_advance_in_lockstep() {
        assert_full_mesh_lockstep(3, 9201, 12);
    }

    #[test]
    fn four_participants_in_a_full_mesh_advance_in_lockstep() {
        assert_full_mesh_lockstep(4, 9202, 12);
    }

    /// `target_ticks`まで全員を進める。進まなければ実装の不具合とみなす。
    fn pump_all_to(states: &mut [BattleState], target_ticks: u32) {
        for _ in 0..MAX_PUMPS {
            for state in states.iter_mut() {
                pump(state, target_ticks, None);
            }
            if states.iter().all(|state| state.net_tick >= target_ticks) {
                return;
            }
            pump_interval();
        }
    }

    #[test]
    fn a_participant_leaving_with_bye_is_ranked_last_while_the_others_keep_playing() {
        // 4人のうち1人がByeを送って抜けても、残り3人だけで対戦が進み、抜けた人は
        // 最下位で確定する(#274設計書3節)。
        const N: usize = 4;
        const TICKS_BEFORE: u32 = 3;
        const TICKS_AFTER: u32 = 9;
        const LEAVER: usize = N - 1;

        let mut states = connected_mesh(N, 9203);
        pump_all_to(&mut states, TICKS_BEFORE);
        assert!(
            states.iter().all(|state| state.net_tick == TICKS_BEFORE),
            "前提: まずは全員が同じtickまで進むはず"
        );

        // 抜ける側はByeを送ってから状態を捨てる(接続も閉じる)。
        let mut leaver = states.pop().expect("4人ぶんあるはず");
        leaver.notify_bye();
        drop(leaver);

        pump_all_to(&mut states, TICKS_AFTER);
        for (h, state) in states.iter().enumerate() {
            let leaver_index = games_index_of(N, h, LEAVER);
            assert_eq!(
                state.net_tick, TICKS_AFTER,
                "参加者{h}: 残った3人だけで対戦が進むはず"
            );
            assert!(
                link_at(state, leaver_index - 1).disconnected,
                "参加者{h}: 抜けた参加者は切断扱いになるはず"
            );
            assert_eq!(
                state.games[leaver_index].status,
                GameStatus::GameOver,
                "参加者{h}: 順位確定のため抜けた参加者の盤面はGameOverへ倒すはず"
            );
            assert_eq!(
                state.ranks[leaver_index],
                Some(N as u8),
                "参加者{h}: 抜けた参加者は最下位で確定するはず"
            );
            assert_eq!(
                state.ranks[0], None,
                "参加者{h}: 生存者の順位はまだ未確定のはず"
            );
            assert_eq!(
                state.outcome, None,
                "参加者{h}: 生存者が3人残っているので決着はしないはず"
            );
            assert_eq!(
                state.games[0].status,
                GameStatus::Playing,
                "参加者{h}: 自分は続けてプレイできるはず"
            );
        }
    }

    #[test]
    fn a_participant_that_stops_sending_input_is_ranked_last_while_the_others_keep_playing() {
        // 1人が入力を送らなくなった場合も同様に、待機上限を超えた時点で最下位で確定し、
        // 残りの参加者で対戦を続ける。接続自体は生かしたままにして、切断検知ではなく
        // tick待ちのタイムアウトで脱落することを見る。
        const N: usize = 4;
        const TICKS_BEFORE: u32 = 2;
        const TICKS_AFTER: u32 = 6;
        const SILENT: usize = N - 1;

        let mut states = connected_mesh(N, 9204);
        pump_all_to(&mut states, TICKS_BEFORE);
        assert!(
            states.iter().all(|state| state.net_tick == TICKS_BEFORE),
            "前提: まずは全員が同じtickまで進むはず"
        );
        let _silent = states.pop().expect("4人ぶんあるはず");

        let started = Instant::now();
        let deadline = Duration::from_millis(LOCKSTEP_WAIT_TIMEOUT_MS * 8);
        while started.elapsed() < deadline {
            for state in states.iter_mut() {
                pump(state, TICKS_AFTER, None);
            }
            if states.iter().all(|state| state.net_tick >= TICKS_AFTER) {
                break;
            }
            pump_interval();
        }

        assert!(
            started.elapsed() >= Duration::from_millis(LOCKSTEP_WAIT_TIMEOUT_MS),
            "待機上限に達する前に脱落扱いにはしないはず"
        );
        for (h, state) in states.iter().enumerate() {
            let silent_index = games_index_of(N, h, SILENT);
            assert_eq!(
                state.net_tick, TICKS_AFTER,
                "参加者{h}: 待機上限の後は残りの参加者で進み続けるはず"
            );
            assert!(
                link_at(state, silent_index - 1).disconnected,
                "参加者{h}: 入力の途絶えた参加者は切断扱いになるはず"
            );
            assert_eq!(
                state.ranks[silent_index],
                Some(N as u8),
                "参加者{h}: 入力の途絶えた参加者は最下位で確定するはず"
            );
            assert_eq!(
                state.outcome, None,
                "参加者{h}: 生存者が3人残っているので決着はしないはず"
            );
        }
    }

    #[test]
    fn a_mismatching_state_hash_from_any_peer_ends_the_battle_as_a_desync() {
        // どのpeerとの照合で食い違ってもデシンクになる(#274設計書6節)。
        const N: usize = 4;
        for bad in 0..N - 1 {
            let (mut host, mut peers) = battle_with_raw_peers(N, 9205);

            // 相手役は全員、tick 0を完了させるための入力だけ送る。
            for peer in peers.iter_mut() {
                net::write_message(
                    peer,
                    &GameMessage::Input {
                        tick: 0,
                        action: NetAction::None,
                    },
                )
                .unwrap();
            }
            for _ in 0..MAX_PUMPS {
                pump(&mut host, 1, None);
                if host.net_tick >= 1 {
                    break;
                }
                pump_interval();
            }
            assert_eq!(host.net_tick, 1, "前提: tick 0が完了しているはず");

            // `bad`番目のpeerだけ、自分の盤面として食い違う値を申告する。
            let (mine_local, mine_remote) = link_at(&host, bad).own_state_hashes[&0];
            net::write_message(
                &mut peers[bad],
                &GameMessage::StateHash {
                    tick: 0,
                    local_hash: mine_remote ^ 1,
                    remote_hash: mine_local,
                },
            )
            .unwrap();
            for _ in 0..MAX_PUMPS {
                host.advance(Duration::ZERO, None);
                if host.outcome.is_some() {
                    break;
                }
                pump_interval();
            }

            assert_eq!(
                host.outcome,
                Some(BattleOutcome::Desync),
                "peer{bad}との不一致でもデシンクになるはず"
            );
        }
    }
}
