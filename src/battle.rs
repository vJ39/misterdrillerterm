//! 対戦用の状態(#252/#254/#273。spec.md 12章)。
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
//! 置き換えた。通信あり(`network`)の経路は2人専用のまま残っており、N人分の接続
//! (フルメッシュ)への置き換えは段階B(#274)で行う。

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
    /// `Some`なら実際の通信で相手の入力を得る(#254)。`None`なら#252までと同じ、
    /// 他の参加者の入力は常に`None`として扱うローカル専用の動作(#273のテストの前提)。
    ///
    /// この経路は2人専用のまま残しており、`games`の長さが2であることを前提とする。
    /// N人分のフルメッシュ接続への置き換えは段階B(#274)で行う。
    network: Option<NetworkLink>,
}

/// 対戦相手との通信路(#254)。`BattleState`が対戦中ずっと保持する。
struct NetworkLink {
    /// 送信用のストリーム。送信はメインループから直接行い、専用スレッドは立てない
    /// (TCPの送信バッファへ書くだけで通常は即座に返るため)。
    writer: TcpStream,
    /// 受信専用スレッドからのイベントキュー。
    event_rx: mpsc::Receiver<NetworkEvent>,
    /// Dropさせない目的だけで保持する(スレッド自体は`writer`と無関係に動く)。
    _receiver_thread: thread::JoinHandle<()>,
    /// 次に処理するtick番号。`Input`メッセージの`tick`と突き合わせるのに使う。
    next_tick: u32,
    /// `next_tick`のぶんとして既に送信済みの自分の入力。相手の入力を待っている間に
    /// 自分の入力だけ先に確定・送信するため、tickが揃うまでここに控える
    /// (`None`なら`next_tick`ぶんの自分の入力はまだ確定していない)。
    committed_local_action: Option<NetAction>,
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
            network: None,
        }
    }

    /// #253のハンドシェイク結果と確立済みのTCPストリームから、通信ありの対戦状態を
    /// 組み立てる(#254)。`stream`は呼び出し元がハンドシェイクに使ったものをそのまま渡す
    /// (内部で`try_clone`して読み書き用に分ける)。
    ///
    /// 呼び出し元はロビー(`lobby.rs`)で、招待の成立後にホスト役・クライアント役の
    /// どちらの経路からもここへ合流する(#256)。`my_name`は自分の表示名で、
    /// `player_names`のindex 0に入る(ハンドシェイク結果は相手の名前しか持たないため
    /// 呼び出し元から受け取る)。
    ///
    /// この経路は2人対戦のまま(`games`の長さは2)で、N人分のフルメッシュ接続の確立は
    /// 段階C(#275)で行う。
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
        let player_count = games.len();

        let reader_stream = stream.try_clone()?;
        let (tx, event_rx) = mpsc::channel();
        let receiver_thread = net::spawn_receiver_thread(reader_stream, tx);

        let now = Instant::now();
        Ok(Self {
            games,
            player_names,
            net_tick_accum: Duration::ZERO,
            ranks: vec![None; player_count],
            next_win_rank: 1,
            next_lose_rank: player_count as u8,
            outcome: None,
            network: Some(NetworkLink {
                writer: stream,
                event_rx,
                _receiver_thread: receiver_thread,
                next_tick: 0,
                committed_local_action: None,
                pending_remote_inputs: VecDeque::new(),
                awaiting_since: None,
                last_remote_activity: now,
                last_heartbeat_sent: now,
                start_at_unix_ms: handshake.start_at_unix_ms,
                result_sent: false,
                remote_result: None,
                disconnected: false,
                own_state_hashes: HashMap::new(),
                pending_remote_state_hashes: HashMap::new(),
            }),
        })
    }

    /// 決着(#256)。`Some`なら対戦は終わっており、画面側は結果表示へ切り替える。
    pub fn outcome(&self) -> Option<BattleOutcome> {
        self.outcome
    }

    /// 対戦から抜けることを相手へ伝える(#256)。通信なし(#252のローカル専用)の場合や
    /// 既に切断されている場合は何も起きない。届かなくても相手側はHeartbeatの途絶で
    /// 切断を検知するため、送信失敗は無視する。
    pub fn notify_bye(&mut self) {
        let Some(link) = &mut self.network else {
            return;
        };
        let _ = net::write_message(&mut link.writer, &GameMessage::Bye);
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
    /// 通信あり(`network`が`Some`)の場合は`advance_networked`へ委ねる。相手の入力が
    /// 揃ったtickしか進められないため、時間の扱いがローカル専用の場合と異なる。
    pub fn advance(&mut self, delta: Duration, local_action: Option<InputAction>) {
        if self.network.is_some() {
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

    /// 通信ありの1フレーム(#254)。相手の入力が揃ったtickだけを進める。
    ///
    /// 相手を待っている間は`net_tick_accum`へ時間を足さない(自分だけ時計が進むと
    /// lockstepの前提が壊れる)。1回の呼び出しで複数tick進む場合も、待機に入った時点で
    /// 残りのtickは次回の呼び出しへ持ち越す。
    fn advance_networked(&mut self, delta: Duration, local_action: Option<InputAction>) {
        // 受信処理だけは決着後も続ける(相手の`Result`は自分の決着より後に届くため)。
        self.drain_network_events();
        if self.outcome.is_some() {
            self.reconcile_remote_result();
            return;
        }

        let link = self.network.as_mut().expect("通信ありの経路でのみ呼ばれる");
        if link.disconnected {
            // 切断を検知した側の不戦勝(12.4)。盤面から導けない決着のため`ranks`は触らず
            // 自分の順位だけを1位として確定する(#274でN人版へ作り直す)。
            self.outcome = Some(BattleOutcome::Ranked(1));
            return;
        }
        if let Some(since) = link.awaiting_since {
            if since.elapsed() >= Duration::from_millis(LOCKSTEP_WAIT_TIMEOUT_MS) {
                link.disconnected = true;
                self.outcome = Some(BattleOutcome::Ranked(1));
            }
            // 待機中は自分の時計を進めない(12.3)。届いていれば`drain_network_events`が
            // 既に待機を解除している。
            return;
        }
        if link.last_heartbeat_sent.elapsed() >= Duration::from_millis(HEARTBEAT_INTERVAL_MS) {
            let tick = link.next_tick;
            link.last_heartbeat_sent = Instant::now();
            let _ = net::write_message(&mut link.writer, &GameMessage::Heartbeat { tick });
        }

        self.net_tick_accum += delta.min(Duration::from_millis(DELTA_CLAMP_MS));

        let net_tick = Duration::from_millis(NET_TICK_MS);
        let mut local_action = local_action;
        while self.net_tick_accum >= net_tick {
            let link = self.network.as_mut().expect("通信ありの経路でのみ呼ばれる");

            // 自分の入力は相手を待たずに先に確定して送る。相手の入力が届いてから送る形に
            // すると、両者が相手の`Input`を待ったまま進まなくなる。送信済みの入力は
            // tickが揃うまで`committed_local_action`に控え、同じtickを二重に送らない。
            let my_action = match link.committed_local_action {
                Some(action) => action,
                None => {
                    let action = local_action
                        .take()
                        .and_then(Option::<NetAction>::from)
                        .unwrap_or(NetAction::None);
                    link.committed_local_action = Some(action);
                    let _ = net::write_message(
                        &mut link.writer,
                        &GameMessage::Input {
                            tick: link.next_tick,
                            action,
                        },
                    );
                    action
                }
            };

            let Some(remote_action) = take_remote_input_for(link, link.next_tick) else {
                // 相手の入力がまだ無い。このtickぶんは`net_tick_accum`から引かずに
                // 次回の呼び出しへ持ち越す。
                link.awaiting_since = Some(Instant::now());
                break;
            };

            self.net_tick_accum -= net_tick;
            link.committed_local_action = None;
            link.next_tick += 1;
            let completed_tick = link.next_tick - 1; // このtickの処理が完了した
            self.run_net_tick_with_actions(&[my_action.into(), remote_action.into()]);

            // 定期的に状態ダイジェストを交換してデシンクを検出する(#255。spec.md 12.3)。
            // tick 0も対象になり、そこでの照合はハンドシェイクで合意したseed/configから
            // 同一の初期盤面が作られているかの検証を兼ねる。
            if completed_tick.is_multiple_of(STATE_HASH_INTERVAL_TICKS) {
                let local_hash = self.games[0].state_hash();
                let remote_hash = self.games[1].state_hash();
                let link = self.network.as_mut().expect("通信ありの経路でのみ呼ばれる");
                link.own_state_hashes
                    .insert(completed_tick, (local_hash, remote_hash));
                let _ = net::write_message(
                    &mut link.writer,
                    &GameMessage::StateHash {
                        tick: completed_tick,
                        local_hash,
                        remote_hash,
                    },
                );
                reconcile_state_hash(link, &mut self.outcome, completed_tick);
            }

            if self.outcome.is_some() {
                self.maybe_send_result();
                self.reconcile_remote_result();
                break;
            }
        }
    }

    /// 通信スレッドから届いたイベントを、キューが空になるまで処理する(#254)。
    fn drain_network_events(&mut self) {
        let Some(link) = &mut self.network else {
            return;
        };

        while let Ok(event) = link.event_rx.try_recv() {
            match event {
                NetworkEvent::Message(GameMessage::Input { tick, action }) => {
                    link.last_remote_activity = Instant::now();
                    link.pending_remote_inputs.push_back((tick, action));
                }
                NetworkEvent::Message(GameMessage::Heartbeat { .. }) => {
                    link.last_remote_activity = Instant::now();
                }
                NetworkEvent::Message(GameMessage::StateHash {
                    tick,
                    local_hash,
                    remote_hash,
                }) => {
                    link.last_remote_activity = Instant::now();
                    link.pending_remote_state_hashes
                        .insert(tick, (local_hash, remote_hash));
                    // 自分が先に計算済みで相手の到着を待っていた場合は、ここで照合できる。
                    reconcile_state_hash(link, &mut self.outcome, tick);
                }
                NetworkEvent::Message(GameMessage::Result {
                    reached_goal, tick, ..
                }) => {
                    link.remote_result = Some((reached_goal, tick));
                }
                NetworkEvent::Message(GameMessage::Bye) | NetworkEvent::Disconnected => {
                    link.disconnected = true;
                }
                // ハンドシェイク用のメッセージは#253で消費済みのため、この段階で
                // 届いても無視してよい。
                NetworkEvent::Message(_) => {}
            }
        }

        // 待っていたtickの入力が届いていれば待機を解除する。
        if link.awaiting_since.is_some()
            && link
                .pending_remote_inputs
                .iter()
                .any(|&(tick, _)| tick == link.next_tick)
        {
            link.awaiting_since = None;
        }

        // Input・Heartbeatのいずれも途絶えたら切断とみなす(12.4)。
        if link.last_remote_activity.elapsed() >= Duration::from_millis(HEARTBEAT_TIMEOUT_MS) {
            link.disconnected = true;
        }
    }

    /// 決着直後に自分の`Result`を1回だけ送る(12.4。勝敗判定の根拠ではなく相互確認用)。
    fn maybe_send_result(&mut self) {
        let reached_goal = self.games[0].status == GameStatus::Cleared;
        let Some(link) = &mut self.network else {
            return;
        };
        if link.result_sent {
            return;
        }
        link.result_sent = true;

        let time_ms = net::unix_time_ms().saturating_sub(link.start_at_unix_ms);
        let _ = net::write_message(
            &mut link.writer,
            &GameMessage::Result {
                reached_goal,
                tick: link.next_tick,
                time_ms,
            },
        );
    }

    /// 受信済みの相手の`Result`を自分のシミュレーション結果と照合する(12.4)。
    ///
    /// 相手の自己申告と、自分が持つ相手インスタンスの判定が食い違ったら、どちらが正しいか
    /// 判定できないため`Desync`へ上書きする(順位方式では「順位が確定しない終わり方」が
    /// `Desync`にあたる。2人専用だった頃は同じ意図を`Draw`で表していた)。自分がまだ
    /// 決着していない段階では、単に自分のtickが相手より遅れているだけのため照合しない。
    fn reconcile_remote_result(&mut self) {
        if self.outcome.is_none() {
            return;
        }
        let reached_goal_in_my_simulation = self.games[1].status == GameStatus::Cleared;
        let Some(link) = &self.network else {
            return;
        };
        let Some((remote_reached_goal, _tick)) = link.remote_result else {
            return;
        };

        if remote_reached_goal != reached_goal_in_my_simulation {
            self.outcome = Some(BattleOutcome::Desync);
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

/// `pending_remote_inputs`から指定したtickの相手の入力を取り出す。届く順序は通常
/// tick順だが、取り違えを防ぐためtick番号で突き合わせる。
fn take_remote_input_for(link: &mut NetworkLink, tick: u32) -> Option<NetAction> {
    let index = link
        .pending_remote_inputs
        .iter()
        .position(|&(pending_tick, _)| pending_tick == tick)?;
    link.pending_remote_inputs
        .remove(index)
        .map(|(_, action)| action)
}

/// 指定tickについて、自分の計算値と相手からの申告値が両方揃っていれば照合する(#255)。
/// 相手の`local_hash`(相手自身の盤面)は自分の`games[1]`のそのtick時点の値と、
/// 相手の`remote_hash`(相手から見た自分)は自分の`games[0]`のそのtick時点の値と
/// 一致するはず。不一致ならデシンクとして`outcome`を`Desync`にする(spec.md 12.3)。
///
/// 送信時(自分がそのtickへ到達した時)と受信時の両方から呼ぶ。どちらが先になるかは
/// 通信の遅延次第のため、両方が揃った側の呼び出しだけが実際の照合まで進む。
fn reconcile_state_hash(link: &mut NetworkLink, outcome: &mut Option<BattleOutcome>, tick: u32) {
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
        // なるため、150msのtickで十分な回数を回す。
        advance_until_outcome(&mut state, 60);

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

        advance_until_outcome(&mut state, 60);

        assert_eq!(state.games[0].status, GameStatus::GameOver);
        assert_eq!(state.outcome, Some(BattleOutcome::Ranked(2)));
        assert_eq!(state.ranks, vec![Some(2), Some(1)]);
    }

    #[test]
    fn frame_deltas_shorter_than_one_net_tick_are_carried_over() {
        // 150msに満たないフレームではtickが起きず、繰り越した分と合わせて150msを
        // 超えた時点で1tick進む。
        let mut state = battle(8, 9);

        state.advance(Duration::from_millis(100), None);
        assert_eq!(
            state.games[0].debug_frame(),
            0,
            "150msに満たないのでまだtickは起きないはず"
        );
        assert_eq!(state.net_tick_accum, Duration::from_millis(100));

        state.advance(Duration::from_millis(100), None);
        assert_eq!(
            state.games[0].debug_frame(),
            1,
            "繰り越し分と合わせて150msを超えたら1tick進むはず"
        );
        assert_eq!(
            state.net_tick_accum,
            Duration::from_millis(50),
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
            1,
            "250msにクランプされるので1tickぶんしか進まないはず"
        );
        assert_eq!(state.net_tick_accum, Duration::from_millis(100));
    }

    #[test]
    fn only_the_first_tick_of_a_frame_consumes_the_local_action() {
        // 1フレームで2tick進む場合でも、自分の入力が適用されるのは最初の1tickだけ。
        // 同じtick列を1tickずつ手で回したものと状態が一致することで確認する。
        let mut batched = battle(12, 13);
        batched.advance(Duration::from_millis(250), None); // 1tick進み100ms繰り越す
        batched.advance(Duration::from_millis(250), Some(InputAction::MoveRight)); // 2tick進む

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

    /// 通信ありの状態が持つ`NetworkLink`を取り出す。
    fn link(state: &BattleState) -> &NetworkLink {
        state.network.as_ref().expect("通信ありの対戦状態のはず")
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

    /// 対戦の1フレームぶん`advance`を呼ぶ。実測時間を渡すのは「目標tickにまだ達して
    /// おらず、前フレームぶんの蓄積も使い切っている」ときだけにする。こうしないと相手待ちの
    /// 空回り中に時間だけが溜まり、後からまとめてtickへ化けて両者のtick数がずれる。
    fn pump(state: &mut BattleState, target_ticks: u32, action: Option<InputAction>) {
        let needs_time = state.net_tick_accum < net_tick() && link(state).next_tick < target_ticks;
        let delta = if needs_time {
            net_tick()
        } else {
            Duration::ZERO
        };
        state.advance(delta, action);
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
}
