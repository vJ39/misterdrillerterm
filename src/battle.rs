//! 対戦用の状態(#252/#254/#273/#274。spec.md 12章)。
//!
//! 「全参加者の盤面を自分の実測フレーム時間で進め、決着を確定する」状態遷移を持つ。
//! #252では通信を伴わない状態遷移だけだったが、#254で実際のTCP通信(#253)と繋ぎ、
//! 通信スレッドとのInput交換・切断検知・`Result`の交換を追加した。#256でロビー
//! (`lobby.rs`)から`Screen::Battle`へ到達する入口ができたが、2台での実プレイを
//! 自動テストでは回せないため、検証は引き続きループバックTCPを使ったユニットテストで行う。
//!
//! #273で参加者を2人固定からN人(2〜4)へ一般化した。盤面は`games: Vec<Game>`(index 0が
//! 自分)で持ち、決着は「Win/Lose/Draw」の3値から順位(`BattleOutcome::Ranked`)へ
//! 置き換えた。#274で通信経路もN人(フルメッシュ)へ広げ、自分以外の各参加者と1本ずつ
//! TCP接続を持つ(`peers`)形にした。Input/Heartbeat/Attack/Result/Byeの交換は2人版の
//! ロジックをpeerごとに適用している。
//!
//! #299で固定tickのlockstepを廃止した。全員の入力がそろうのを待って論理tickを刻む形は、
//! 何らかの理由でシミュレーションが1箇所でも食い違うと対戦そのものが中断してしまう
//! (デシンク)。代わりに各参加者が自分の実時間で自分の盤面を進め、受け取った操作は
//! 届いた瞬間に反映する非同期方式にした。盤面の完全一致は前提にしないため、デシンク
//! という失敗の仕方自体が無くなる(spec.md 12.3)。

use std::io;
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::autoplay::Autopilot;
use crate::constants::{HEARTBEAT_INTERVAL_MS, HEARTBEAT_TIMEOUT_MS};
use crate::game::{Game, GameEvent, GameStatus, InputAction};
use crate::net::{self, BattleConfig, GameMessage, NetAction, NetworkEvent};

/// 1フレームの実測時間としてシミュレーションへ渡す上限(ms)。通常プレイ(`tick_playing`)が
/// `Game::update`へ渡すdeltaに掛けているクランプと同じ値で、ウィンドウ非アクティブ等で
/// 大きく空いたフレームが一度に大量の時間を進めてしまうのを防ぐ。
const DELTA_CLAMP_MS: u64 = 250;

/// 対戦の決着(#273。spec.md 12.4)。自分視点の最終順位で表す(2人版のWin=1位/
/// Lose=2位/Draw=同着への一般化)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleOutcome {
    /// 自分の最終順位(1が1位)。同着は同じ順位になる。
    Ranked(u8),
}

/// 対戦画面(`Screen::Battle`)が持つ状態。
///
/// `games`の各要素は通常プレイと同じ`Game`で、自分の盤面(index 0)は自分の操作、他の
/// 参加者の盤面はその参加者から届いた操作で進める。全員を同じ`delta`で進めるが、
/// 参加者間で盤面が完全に一致することは前提にしない(spec.md 12.3)。
pub struct BattleState {
    /// 自分を含む全参加者の盤面。index 0が自分。フルメッシュ接続(#273設計書1節)なので、
    /// 対戦中は全参加者の視点をローカルに保持する。
    pub games: Vec<Game>,
    /// このフレームまでに自分(`games[0]`)が発生させた`GameEvent`(#295)。呼び出し元
    /// (`tick_battle`)がSE再生に使うため、消費されるまで溜めておく。
    pending_local_events: Vec<GameEvent>,
    /// AI操作主体。`games[1..]`と同じ並びで対応し、AIの枠だけ`Some`。空なら
    /// AIのいない対戦(ローカル専用/通信あり)。
    ///
    /// ローカルAI対戦(#296)では全要素が`Some`になる。#300でルームへAIを追加できるように
    /// したため、`peers`と両方`Some`(通信ありのルームにAIが混ざる)という組み合わせも
    /// あり得る。その場合`Some`なのはホスト(room内インデックス0)だけで、ゲストは
    /// ホストからの代理送信でAIの盤面を進めるため`None`のままにする(両方が`decide`を
    /// 呼ぶと同じAIの操作が二重に適用されてしまう)。
    ai_pilots: Vec<Option<Autopilot>>,
    /// AIの枠ごとの`Result`代理送信済みフラグ(#300)。`ai_pilots`と同じ並びで対応する。
    ai_result_sent: Vec<bool>,
    /// 各参加者の表示名。`games`と同じindexで対応する(index 0が自分)。
    pub player_names: Vec<String>,
    /// 確定した順位(1が1位)。`games`と同じindexで対応する。全参加者が結果
    /// (`Cleared`/`GameOver`)を出すまでは全員`None`のままで、そろった時点で一括で
    /// 確定する(#289。スコア・到達深度を基準にするには全員の最終結果を比較する必要が
    /// あるため、#273時点の「各自が結果を出した瞬間に確定」という設計から変更した)。
    ranks: Vec<Option<u8>>,
    /// 決着。`Some`になった以後は盤面を進めず、自分の入力も受け付けない。
    outcome: Option<BattleOutcome>,
    /// 自分以外の各参加者との通信路(#274)。`peers[i]`は`games[i+1]`に対応する
    /// (自分が`games[0]`なので、`games[1..]`と`peers[0..]`が1対1)。`None`なら通信なしで、
    /// 他の参加者は操作されないローカル専用の動作(#252/#273のテストと#296のAI対戦の前提)。
    ///
    /// `peers[i]`が`None`なら`games[i+1]`はAI(#300)で、TCP接続を持たない。
    peers: Option<Vec<Option<PeerLink>>>,
    /// 自分のroom内インデックス(#300)。`games`上のindexとroom内インデックスの変換
    /// (`games_index_for_room_index`/`room_index_for_games_index`)に使う。通信なしの
    /// 対戦では意味を持たないため0。
    my_room_index: usize,
    /// 対戦開始時刻(ハンドシェイクの`StartCountdown`の値)。AI(#300)の`Result`を代理送信
    /// するときの経過時間の算出に使う(`PeerLink::start_at_unix_ms`と同じ値)。
    start_at_unix_ms: u64,
}

/// 対戦相手1人との通信路(#274。#254の`NetworkLink`を複数保持できるよう改名した)。
/// `BattleState`が対戦中ずっと保持する。
struct PeerLink {
    /// 送信用のストリーム。送信はメインループから直接行い、専用スレッドは立てない
    /// (TCPの送信バッファへ書くだけで通常は即座に返るため)。
    writer: TcpStream,
    /// 受信専用スレッドからのイベントキュー。
    event_rx: mpsc::Receiver<NetworkEvent>,
    /// Dropさせない目的だけで保持する(スレッド自体は`writer`と無関係に動く)。
    _receiver_thread: thread::JoinHandle<()>,
    /// 相手から何らかのメッセージを最後に受信した時刻(`HEARTBEAT_TIMEOUT_MS`の判定に使う)。
    last_remote_activity: Instant,
    /// 最後にHeartbeatを送信した時刻。
    last_heartbeat_sent: Instant,
    /// 対戦開始時刻(ハンドシェイクの`StartCountdown`の値)。Result送信時の
    /// `time_ms`(経過時間)の算出に使う。
    start_at_unix_ms: u64,
    /// 自分のResultは送信済みか。決着直後に一度だけ送るためのフラグ。
    result_sent: bool,
    /// 相手の切断を検知済みか(Bye受信・ソケットエラー・受信の途絶)。
    disconnected: bool,
}

impl PeerLink {
    /// 確立済みのTCP接続1本から通信路を組み立てる(#274)。`stream`は呼び出し元が
    /// ハンドシェイクに使ったものをそのまま渡す(内部で`try_clone`して読み書き用に分け、
    /// 読み側は受信専用スレッドへ預ける)。
    fn new(stream: TcpStream, start_at_unix_ms: u64) -> io::Result<Self> {
        // Input/Attack/Result/Heartbeatは1件あたり数十バイトの小さいメッセージを
        // 頻繁に送り合う。Nagleアルゴリズムが有効だと、直前の送信のACKを待つ間ここが
        // バッファされ、操作の反映までの遅延が積み重なる(#287、実機で「対戦がまだ重い」
        // と報告された原因の一つ)。
        stream.set_nodelay(true)?;
        let reader_stream = stream.try_clone()?;
        let (tx, event_rx) = mpsc::channel();
        let receiver_thread = net::spawn_receiver_thread(reader_stream, tx);

        let now = Instant::now();
        Ok(Self {
            writer: stream,
            event_rx,
            _receiver_thread: receiver_thread,
            last_remote_activity: now,
            last_heartbeat_sent: now,
            start_at_unix_ms,
            result_sent: false,
            disconnected: false,
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
            pending_local_events: Vec::new(),
            ai_pilots: Vec::new(),
            ai_result_sent: Vec::new(),
            player_names,
            ranks: vec![None; player_count],
            outcome: None,
            peers: None,
            my_room_index: 0,
            start_at_unix_ms: 0,
        }
    }

    /// ローカルAI対戦(#296)。通信を一切使わず、`human_game`(自分)と`ai_games`(AI操作)を
    /// 通信ありの場合と同じ`advance`でローカルに進める。`ai_games`は
    /// `new_game_from_battle_config`で人間と同じシード・設定から作ったものを渡す
    /// (地形とアイテム配置を揃えるため。spec.md 12.2と同じ考え方)。
    pub fn new_local_vs_ai(human_game: Game, ai_games: Vec<Game>, my_name: String) -> Self {
        // AIは無敵に頼らず素で戦わせる(#221の通常AIをそのまま使う)。`Autopilot::new`の
        // 引数は「オートプレイを抜けるときに戻す無敵状態」で、対戦では抜ける操作が
        // 無いため常にfalseでよい。
        let ai_pilots: Vec<Option<Autopilot>> = ai_games
            .iter()
            .map(|_| Some(Autopilot::new(false)))
            .collect();
        let ai_result_sent = vec![false; ai_pilots.len()];
        let mut games = Vec::with_capacity(ai_games.len() + 1);
        games.push(human_game);
        games.extend(ai_games);
        let mut player_names = Vec::with_capacity(games.len());
        player_names.push(my_name);
        for i in 1..games.len() {
            player_names.push(format!("AI {i}"));
        }
        let player_count = games.len();
        Self {
            games,
            pending_local_events: Vec::new(),
            ai_pilots,
            ai_result_sent,
            player_names,
            ranks: vec![None; player_count],
            outcome: None,
            peers: None,
            my_room_index: 0,
            start_at_unix_ms: 0,
        }
    }

    /// N人分の確立済みTCP接続から通信ありの対戦状態を組み立てる(#274)。
    ///
    /// `games`/`player_names`は既にseed/configから生成済み(index 0が自分)で、`streams`は
    /// 自分以外の各参加者との接続(`games`のindex 1..と対応する順)。`start_at_unix_ms`は
    /// ハンドシェイクで合意した開始時刻(`Result`送信時の経過時間の算出に使う)。
    ///
    /// #300: `streams[i]`が`None`なら`games[i+1]`はAI(接続を持たない追加参加者)。ホスト
    /// (`my_index`が0)だけがそのAIをローカルで動かし、入力・妨害岩・結果を代理送信する。
    /// `my_index`は自分のroom内インデックスで、代理送信の宛先変換に使う。
    ///
    /// 接続の確立自体(誰が誰へ繋ぐか・configとseedの配布)は呼び出し元の責務で、N人分の
    /// ハンドシェイクは段階C(#275)で実装する。
    pub fn from_peer_streams(
        games: Vec<Game>,
        player_names: Vec<String>,
        streams: Vec<Option<TcpStream>>,
        my_index: usize,
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
        // #300: AIの枠(接続なし)を動かすのはホストだけ。ゲストはホストからの代理送信で
        // その盤面を進めるため`Autopilot`を持たない。
        let ai_pilots: Vec<Option<Autopilot>> = streams
            .iter()
            .map(|stream| {
                // `Autopilot::new`の引数は`new_local_vs_ai`と同じ理由で常にfalse。
                (stream.is_none() && my_index == 0).then(|| Autopilot::new(false))
            })
            .collect();
        let ai_result_sent = vec![false; ai_pilots.len()];
        let peers = streams
            .into_iter()
            .map(|stream| {
                stream
                    .map(|stream| PeerLink::new(stream, start_at_unix_ms))
                    .transpose()
            })
            .collect::<io::Result<Vec<Option<PeerLink>>>>()?;

        Ok(Self {
            games,
            pending_local_events: Vec::new(),
            ai_pilots,
            ai_result_sent,
            player_names,
            ranks: vec![None; player_count],
            outcome: None,
            peers: Some(peers),
            my_room_index: my_index,
            start_at_unix_ms,
        })
    }

    /// room内インデックスを自分の`games`上のindexへ変換する(#300)。自分は常に`games[0]`で、
    /// 自分より前のroom内インデックスは1つ後ろへずれる。
    fn games_index_for_room_index(&self, room_index: usize) -> usize {
        if room_index == self.my_room_index {
            0
        } else if room_index < self.my_room_index {
            room_index + 1
        } else {
            room_index
        }
    }

    /// `games`上のindexをroom内インデックスへ変換する(#300。`games_index_for_room_index`の逆)。
    fn room_index_for_games_index(&self, games_index: usize) -> usize {
        if games_index == 0 {
            self.my_room_index
        } else if games_index <= self.my_room_index {
            games_index - 1
        } else {
            games_index
        }
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

        // 2人版にAI(#300)は混ざらないため、自分のroom内インデックスは0でよい。
        Self::from_peer_streams(
            games,
            player_names,
            vec![Some(stream)],
            0,
            handshake.start_at_unix_ms,
        )
    }

    /// 決着(#256)。`Some`なら対戦は終わっており、画面側は結果表示へ切り替える。
    pub fn outcome(&self) -> Option<BattleOutcome> {
        self.outcome
    }

    /// 自分の盤面の描画用。#299で固定tickを廃止し、自分の操作は受け取った瞬間に
    /// `games[0]`へ適用されるようになったため、先行反映用のコピー(#292)は不要になり、
    /// 常に自分の盤面そのものを返す(呼び出し元は変更しなくてよい)。
    pub fn predicted_game(&self) -> &Game {
        &self.games[0]
    }

    /// 直近のフレームで自分が発生させた`GameEvent`を取り出す(#295)。呼び出し元
    /// (`tick_battle`)がSE再生に使う。取り出した後は空になる。
    pub fn take_local_events(&mut self) -> Vec<GameEvent> {
        std::mem::take(&mut self.pending_local_events)
    }

    /// 対戦から抜けることを他の参加者全員へ伝える(#256/#274)。通信なし(#252のローカル
    /// 専用)の場合は何も起きない。届かなくても相手側はHeartbeatの途絶で切断を検知する
    /// ため、送信失敗は無視する。
    pub fn notify_bye(&mut self) {
        let Some(peers) = &mut self.peers else {
            return;
        };
        // #300: AIの枠(`None`)は接続を持たないため飛ばす。
        for peer in peers.iter_mut().flatten() {
            if peer.disconnected {
                continue;
            }
            let _ = net::write_message(&mut peer.writer, &GameMessage::Bye);
        }
    }

    /// 全参加者が結果(`Cleared`/`GameOver`)を出しそろった時点で、最終順位を一括で
    /// 確定する(#289)。
    ///
    /// ゴール到達者は必ず脱落者より上位。同じ到達状態(両者ともCleared、または両者とも
    /// GameOver)の中では、スコア(`player.score`)降順・同スコアなら到達深度
    /// (`player.depth_m()`)降順で順位を付ける。#273時点は「各自が結果を出した瞬間に
    /// 先着順で確定」だったが、スコア・深度で比較するには全員の最終結果がそろっている
    /// 必要があるため、確定のタイミングをここに一本化した(ユーザー判断: 自分が先に
    /// コースをクリアしても、他の参加者がまだプレイ中の間は自分の最終順位も確定しない)。
    fn update_ranks(&mut self) {
        if self.ranks.iter().all(Option::is_some) {
            return;
        }
        let everyone_is_done = self
            .games
            .iter()
            .all(|g| matches!(g.status, GameStatus::Cleared | GameStatus::GameOver));
        if !everyone_is_done {
            return;
        }

        let mut order: Vec<usize> = (0..self.games.len()).collect();
        order.sort_by(|&a, &b| self.ranking_key(b).cmp(&self.ranking_key(a)));

        let mut rank = 1u8;
        for i in 0..order.len() {
            if i > 0 && self.ranking_key(order[i - 1]) != self.ranking_key(order[i]) {
                rank = i as u8 + 1;
            }
            self.ranks[order[i]] = Some(rank);
        }

        if self.outcome.is_none() {
            self.outcome = self.ranks[0].map(BattleOutcome::Ranked);
        }
    }

    /// 順位比較用のキー(#289)。降順で並べると良い順位が先頭に来るタプル:
    /// (ゴール到達したか, スコア, 到達深度)。
    fn ranking_key(&self, index: usize) -> (bool, u64, usize) {
        let game = &self.games[index];
        (
            game.status == GameStatus::Cleared,
            game.player.score,
            game.player.depth_m(),
        )
    }

    /// 1フレーム進める(#299。spec.md 12.3)。
    ///
    /// `local_action`はこのフレームで確定した自分の入力(無ければ`None`)。固定tickを待たず、
    /// 受け取った瞬間に自分の盤面へ適用し、同時に他の参加者へ送る。他の参加者から届いた
    /// 操作も、そのフレームでそのまま相手の盤面へ適用する。全員の盤面を同じ`delta`で
    /// 進めるが、参加者間で盤面が一致することは前提にしない(自分の画面に見えている他人の
    /// 盤面は、通信の遅延ぶん実際より少し古い)。
    ///
    /// 通信あり(`peers`が`Some`)・ローカル専用(`None`。#252/#273のテストと#296のAI対戦)で
    /// 同じ経路を通る。違いは「他の参加者の操作がどこから来るか」と「妨害岩を送るか
    /// 直接渡すか」だけで、時間の進め方は共通。決着後(`outcome`が`Some`)は盤面を進めない。
    pub fn advance(&mut self, delta: Duration, local_action: Option<InputAction>) {
        // 受信処理だけは決着後も続ける(他の参加者の`Result`は自分の決着より後に届くため)。
        self.drain_network_events();
        self.send_due_heartbeats();

        // 決着後は結果表示に専念し、盤面も入力も進めない(12.4)。
        if self.outcome.is_some() {
            self.maybe_send_results();
            return;
        }

        if let Some(action) = local_action {
            let events = self.games[0].apply_input(action);
            self.pending_local_events.extend(events);
            self.broadcast_local_input(action);
        }

        // ローカルAI対戦(#296)とルームへ追加したAI(#300)。1フレームにつき1回分の判断を
        // させ、その場で`games[i + 1]`へ直接適用する。decide()が返す複数アクション
        // (向き変更+掘削の組み合わせ等)はまとめて適用してよい。
        //
        // #300: 通信ありならホストがAIの操作を全peerへ代理送信する。`broadcast`は
        // `&mut self`が必要で`ai_pilots`の借用中には呼べないため、送るぶんを溜めてから
        // ループを抜けて送る。
        let networked = self.peers.is_some();
        let mut proxy_inputs: Vec<(usize, InputAction)> = Vec::new();
        for (i, pilot_slot) in self.ai_pilots.iter_mut().enumerate() {
            let Some(pilot) = pilot_slot else {
                continue;
            };
            let ai_actions = pilot.decide(&self.games[i + 1]);
            for ai_action in ai_actions {
                self.games[i + 1].apply_input(ai_action);
                if networked {
                    proxy_inputs.push((i + 1, ai_action));
                }
            }
        }
        for (games_index, action) in proxy_inputs {
            let Some(action) = Option::<NetAction>::from(action) else {
                continue;
            };
            let proxy_for = Some(self.room_index_for_games_index(games_index));
            self.broadcast(&GameMessage::Input { action, proxy_for });
        }

        // 全員を同じdeltaで進める。ウィンドウ非アクティブ等で大きく空いたフレームは
        // 通常プレイ(`tick_playing`)と同じ上限でクランプする。
        let delta = delta.min(Duration::from_millis(DELTA_CLAMP_MS));
        let local_events = self.games[0].update(delta);
        self.pending_local_events.extend(local_events);
        for game in &mut self.games[1..] {
            // 他の参加者ぶんのイベントはSE再生に使わないため捨てる(#295。鳴らすのは
            // 自分の盤面で起きたことだけ)。
            game.update(delta);
        }

        self.exchange_attack_power();
        self.check_results();
    }

    /// 自分の操作を他の参加者全員へ送る(#274。全員へ同じ値を送る)。同期の対象にならない
    /// 操作(`NetAction`へ変換できないもの)は送らない。ローカル専用(`peers`が`None`)の
    /// 場合は何も起きない。
    fn broadcast_local_input(&mut self, action: InputAction) {
        if self.peers.is_none() {
            return;
        }
        let Some(action) = Option::<NetAction>::from(action) else {
            return;
        };
        self.broadcast(&GameMessage::Input {
            action,
            // 自分自身の入力なので代理送信ではない(#300)。
            proxy_for: None,
        });
    }

    /// 未切断の全peerへ同じメッセージを送る(#274)。届かなくても相手側は受信の途絶で
    /// 切断を検知するため、送信失敗は無視する。ローカル専用の場合は何も起きない。
    fn broadcast(&mut self, message: &GameMessage) {
        let Some(peers) = &mut self.peers else {
            return;
        };
        // #300: AIの枠(`None`)は接続を持たないため飛ばす。
        for peer in peers.iter_mut().flatten() {
            if peer.disconnected {
                continue;
            }
            let _ = net::write_message(&mut peer.writer, message);
        }
    }

    /// 妨害岩(#247/#297。spec.md 12.8)を交換する。各自が消したブロック数を、割らずに
    /// 他の参加者へそのまま届ける(N人時の配分ルールはユーザー確認済み)。
    ///
    /// 通信ありの場合は自分が消したぶんを`Attack`で全peerへ送る。受け取った側が自分の
    /// 状態を見て適用するため、送る側は相手の状態を気にしない。自分が持っている他の
    /// 参加者のコピーが溜めたぶんは、正式な`Attack`メッセージと二重に数えないよう
    /// 取り出して捨てる。
    ///
    /// #300: ただしAIの枠は他の誰も`Attack`を送ってくれないため、ホストが自分と同じ
    /// ルール(0より大きいときだけ)で代理送信する。
    fn exchange_attack_power(&mut self) {
        if self.peers.is_some() {
            let amount = self.games[0].take_pending_attack_power();
            let mut proxy_attacks: Vec<(usize, u32)> = Vec::new();
            for i in 1..self.games.len() {
                let pending = self.games[i].take_pending_attack_power();
                let is_my_ai = self.ai_pilots.get(i - 1).is_some_and(Option::is_some);
                if is_my_ai && pending > 0 {
                    proxy_attacks.push((i, pending));
                }
            }
            if amount > 0 {
                self.broadcast(&GameMessage::Attack {
                    amount,
                    proxy_for: None,
                });
            }
            for (games_index, amount) in proxy_attacks {
                let proxy_for = Some(self.room_index_for_games_index(games_index));
                self.broadcast(&GameMessage::Attack { amount, proxy_for });
            }
            return;
        }

        // ローカル専用(#252/#273のテスト・#296のAI対戦)。相手の状態を直接見られるため、
        // 送信側で生存中(Playing)の参加者だけに渡す。
        let pending: Vec<u32> = self
            .games
            .iter_mut()
            .map(Game::take_pending_attack_power)
            .collect();
        for (i, &power) in pending.iter().enumerate() {
            if power == 0 {
                continue;
            }
            for (j, game) in self.games.iter_mut().enumerate() {
                if i != j && game.status == GameStatus::Playing {
                    game.receive_incoming_attack(power);
                }
            }
        }
    }

    /// このフレームの終わりに決着を確認する。切断したpeerを脱落として畳み、全員の結果が
    /// そろっていれば順位を確定し、確定していれば自分の結果を送る。
    fn check_results(&mut self) {
        self.settle_disconnected_peers();
        // #300: AIの枠の結果は、自分の決着とは関係なくその枠が決着した時点で送る。
        self.maybe_send_ai_results();
        self.update_ranks();
        if self.outcome.is_some() {
            self.maybe_send_results();
        }
    }

    /// 切断・タイムアウトを検知したpeerの順位を、まだ未確定なら確定させる(#274設計書3節)。
    ///
    /// フルメッシュの狙い(単一障害点を避ける)に合わせ、1人が抜けても全員終了にはしない。
    /// 該当peerの盤面を`GameOver`へ倒して通常の脱落と同じ経路(`update_ranks`)に乗せる
    /// ことで、切断者は最下位側から順位が埋まり、残りの参加者は対戦を続けられる。
    fn settle_disconnected_peers(&mut self) {
        let peer_count = self.peers.as_ref().map_or(0, Vec::len);
        for i in 0..peer_count {
            // #300: AIの枠(`None`)は接続を持たないため切断し得ない。
            let disconnected = self.peers.as_ref().expect("通信ありの経路でのみ呼ばれる")[i]
                .as_ref()
                .is_some_and(|peer| peer.disconnected);
            if !disconnected || self.ranks[i + 1].is_some() {
                continue;
            }
            // 実際の脱落ではないが、順位確定のためだけにこの状態を使う。
            self.games[i + 1].status = GameStatus::GameOver;
        }
    }

    /// 送信間隔を超えたpeerへHeartbeatを送る(#254の間隔ロジックをpeerごとに適用)。
    /// 操作していない参加者からは`Input`が届かないため、これが生存の証になる。
    fn send_due_heartbeats(&mut self) {
        let interval = Duration::from_millis(HEARTBEAT_INTERVAL_MS);
        let Some(peers) = &mut self.peers else {
            return;
        };
        // #300: AIの枠(`None`)は接続を持たないため飛ばす。
        for peer in peers.iter_mut().flatten() {
            if peer.disconnected || peer.last_heartbeat_sent.elapsed() < interval {
                continue;
            }
            peer.last_heartbeat_sent = Instant::now();
            let _ = net::write_message(&mut peer.writer, &GameMessage::Heartbeat);
        }
    }

    /// 通信スレッドから届いたイベントを、キューが空になるまで処理する(#254)。peerごとに
    /// 独立したキューを持つため、全peerぶんを順に処理する(#274)。
    ///
    /// 受け取った操作・妨害岩・結果は、順番待ちをせずこのフレームでそのまま盤面へ反映する
    /// (#299。spec.md 12.3)。`peers`を借用している間に`games`へ触れないよう、いったん
    /// 反映内容を集めてから借用を解いて適用する。
    fn drain_network_events(&mut self) {
        if self.peers.is_none() {
            return;
        }
        let heartbeat_timeout = Duration::from_millis(HEARTBEAT_TIMEOUT_MS);
        // (送信元peerのgames上のindex, 代理対象のroom内インデックス, 適用する操作)。
        // 届いた順に並ぶ。`games`上のindexへの変換は`peers`の借用を解いた後に行う(#300)。
        let mut remote_inputs: Vec<(usize, Option<usize>, InputAction)> = Vec::new();
        // 全peerから届いた妨害岩の合計(#247/#297)。誰から来たかは区別しない。
        let mut incoming_attack: u32 = 0;
        // (送信元peerのgames上のindex, 代理対象のroom内インデックス, ゴール到達したか)。
        let mut remote_results: Vec<(usize, Option<usize>, bool)> = Vec::new();

        let peers = self.peers.as_mut().expect("通信ありの経路でのみ呼ばれる");
        // #300: AIの枠(`None`)は受信キューを持たないため、`enumerate`の位置だけ保ったまま飛ばす。
        for (i, peer) in peers
            .iter_mut()
            .enumerate()
            .filter_map(|(i, peer_slot)| peer_slot.as_mut().map(|peer| (i, peer)))
        {
            while let Ok(event) = peer.event_rx.try_recv() {
                match event {
                    NetworkEvent::Message(GameMessage::Input { action, proxy_for }) => {
                        peer.last_remote_activity = Instant::now();
                        if let Some(action) = Option::<InputAction>::from(action) {
                            remote_inputs.push((i + 1, proxy_for, action));
                        }
                    }
                    NetworkEvent::Message(GameMessage::Heartbeat) => {
                        peer.last_remote_activity = Instant::now();
                    }
                    // 妨害岩は受け取った側が自分の盤面へ積むため、誰の代理送信
                    // (`proxy_for`)かで適用先は変わらない(#300)。
                    NetworkEvent::Message(GameMessage::Attack { amount, .. }) => {
                        peer.last_remote_activity = Instant::now();
                        incoming_attack = incoming_attack.saturating_add(amount);
                    }
                    NetworkEvent::Message(GameMessage::Result {
                        reached_goal,
                        proxy_for,
                        ..
                    }) => {
                        peer.last_remote_activity = Instant::now();
                        remote_results.push((i + 1, proxy_for, reached_goal));
                    }
                    NetworkEvent::Message(GameMessage::Bye) | NetworkEvent::Disconnected => {
                        peer.disconnected = true;
                    }
                    // ハンドシェイク用のメッセージは#253で消費済みのため、この段階で
                    // 届いても無視してよい。
                    NetworkEvent::Message(_) => {}
                }
            }

            // 何も届かない状態が続いたら切断とみなす(12.4)。
            if peer.last_remote_activity.elapsed() >= heartbeat_timeout {
                peer.disconnected = true;
            }
        }

        for (peer_games_index, proxy_for, action) in remote_inputs {
            let index = self.apply_target_index(peer_games_index, proxy_for);
            // 他の参加者ぶんのイベントはSE再生に使わないため捨てる(#295)。
            self.games[index].apply_input(action);
        }

        // 妨害岩は、送った側ではなく受け取った側が自分の状態を見て適用する(spec.md 12.8)。
        // 既にゴール・ゲームオーバーしている自分には積まない。
        if incoming_attack > 0 && self.games[0].status == GameStatus::Playing {
            self.games[0].receive_incoming_attack(incoming_attack);
        }

        // 相手が自己申告した結果は、自分が持つその参加者のコピーより優先する(12.4)。
        // 盤面は各自が独立に進めるため、自分のコピー側がまだプレイ中のまま止まることが
        // あり、そのままでは`update_ranks`の「全員の結果がそろう」条件を満たせない。
        for (peer_games_index, proxy_for, reached_goal) in remote_results {
            let index = self.apply_target_index(peer_games_index, proxy_for);
            if self.games[index].status != GameStatus::Playing {
                continue;
            }
            self.games[index].status = if reached_goal {
                GameStatus::Cleared
            } else {
                GameStatus::GameOver
            };
        }
    }

    /// 受信したメッセージを適用する`games`上のindexを決める(#300)。
    ///
    /// `proxy_for`が`Some`ならホストがAIの代理で送ってきたものなので、そのroom内
    /// インデックスに対応する盤面へ。`None`なら従来通り送信元(このpeer)の盤面へ。
    /// 変換した結果が範囲外の場合は、壊れた値で別の参加者の盤面を動かさないよう
    /// 送信元の盤面へ落とす。
    fn apply_target_index(&self, peer_games_index: usize, proxy_for: Option<usize>) -> usize {
        let Some(room_index) = proxy_for else {
            return peer_games_index;
        };
        let index = self.games_index_for_room_index(room_index);
        if index == 0 || index >= self.games.len() {
            return peer_games_index;
        }
        index
    }

    /// 決着直後に自分の`Result`を、未切断の各peerへ1回だけ送る(12.4。勝敗判定の根拠では
    /// なく相互確認用)。
    fn maybe_send_results(&mut self) {
        let reached_goal = self.games[0].status == GameStatus::Cleared;
        let Some(peers) = &mut self.peers else {
            return;
        };
        // #300: AIの枠(`None`)は接続を持たないため飛ばす。
        for peer in peers.iter_mut().flatten() {
            if peer.result_sent || peer.disconnected {
                continue;
            }
            peer.result_sent = true;

            let time_ms = net::unix_time_ms().saturating_sub(peer.start_at_unix_ms);
            let _ = net::write_message(
                &mut peer.writer,
                &GameMessage::Result {
                    reached_goal,
                    time_ms,
                    // 自分自身の結果なので代理送信ではない(#300)。
                    proxy_for: None,
                },
            );
        }
    }

    /// ルームへ追加したAI(#300)の`Result`を、その枠が決着した時に一度だけ代理送信する。
    ///
    /// AIの枠は他の参加者から見ると「ホストが操作する追加の参加者」で、自己申告の
    /// `Result`を送ってくるのはホストだけ。これが無いと他の参加者の`update_ranks`が
    /// 「全員が結果を出しそろった」条件を満たせない。自分の`Result`(`maybe_send_results`)と
    /// 違い、自分の決着とは無関係にその枠が決着した時点で送る。
    fn maybe_send_ai_results(&mut self) {
        if self.peers.is_none() {
            return;
        }
        for i in 0..self.ai_pilots.len() {
            if self.ai_pilots[i].is_none() || self.ai_result_sent[i] {
                continue;
            }
            let reached_goal = match self.games[i + 1].status {
                GameStatus::Cleared => true,
                GameStatus::GameOver => false,
                // まだ決着していない枠は次のフレームへ持ち越す。
                _ => continue,
            };
            self.ai_result_sent[i] = true;

            let time_ms = net::unix_time_ms().saturating_sub(self.start_at_unix_ms);
            let proxy_for = Some(self.room_index_for_games_index(i + 1));
            self.broadcast(&GameMessage::Result {
                reached_goal,
                time_ms,
                proxy_for,
            });
        }
    }
}

#[cfg(test)]
/// テストが1フレームとして渡す経過時間。実機のフレーム間隔(`FRAME_INTERVAL_MS`)より
/// 大きめに取り、少ない回数で盤面が進むようにする。
pub(crate) const TEST_FRAME_DELTA: Duration = Duration::from_millis(50);

#[cfg(test)]
impl BattleState {
    /// 対戦の1フレームぶん`advance`を呼ぶ。ループバックで複数の`BattleState`を回す
    /// テスト(この`battle.rs`と、ルーム参加フローの`room.rs`)が共通で使う。
    pub(crate) fn pump_frame(&mut self, action: Option<InputAction>) {
        self.advance(TEST_FRAME_DELTA, action);
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
    // 妨害岩(#247)は対戦専用のルールで、通常プレイでは常に無効(#297で対戦へ接続)。
    game.set_attack_rules_enabled(true);
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
    // アイテム3種は通常プレイでは進行に応じた窓補充(top_up_items_ahead)に任せるが、
    // 対戦では掘るペースが違う相手同士でRNG消費タイミングがずれ、まだ誰も到達していない
    // 深い場所のアイテム配置が食い違ってしまう(公平性の問題)。対戦では開始時に全深度分
    // 確定させる。
    game.precompute_all_items();
    game
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{FIELD_WIDTH_DEFAULT, ROCK_HITS_TO_BREAK};
    use crate::game::board::Cell;
    use std::net::TcpListener;

    /// テスト用の短いコース(ゴール20m)。本番のノーマルコース(1000m)より盤面生成が軽く、
    /// ゴール到達も数フレームで再現できる。
    const TEST_GOAL_M: usize = 20;

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

    /// 直下に足場を置き、左右を空けて、`MoveLeft`/`MoveRight`が1回目から確実に成功する
    /// 状態にする。移動の反映を列(`player.col`)で観測するテスト用。
    fn open_both_sides(game: &mut Game) {
        let row = game.player.row;
        let col = game.player.col;
        game.board.rows[row + 1][col] = Cell::Rock { hits: 0 };
        game.board.rows[row][col - 1] = Cell::Empty;
        game.board.rows[row][col + 1] = Cell::Empty;
    }

    /// 直下を「あと1撃で壊れる岩」にする。下を向いて`Drill`すると岩が1個壊れ、攻撃力が
    /// 1たまる。`add_attack_power`は`game`モジュールの外から呼べないため、妨害岩のテストは
    /// この手順で実際に掘って発生させる。
    fn arm_rock_below(game: &mut Game) {
        let row = game.player.row;
        let col = game.player.col;
        game.board.rows[row + 1][col] = Cell::Rock {
            hits: ROCK_HITS_TO_BREAK - 1,
        };
    }

    /// 相手から受け取った妨害が盤面に届いているか。岩として予告キューへ積まれた場合と、
    /// 岩1個ぶんに足りず攻撃力のまま控えられた場合の両方を拾う。
    fn has_incoming_attack(game: &Game) -> bool {
        !game.incoming_rocks().is_empty() || game.incoming_attack_power() > 0
    }

    /// `predicate`が満たされるまで(または上限`max_frames`まで)1フレームずつ進める。#289で
    /// 決着が全員の結果待ちになったため、決着以外の条件(誰かがゴール・脱落した時点など)で
    /// 止めたいテスト用。
    fn advance_until(
        state: &mut BattleState,
        max_frames: usize,
        predicate: impl Fn(&BattleState) -> bool,
    ) {
        for _ in 0..max_frames {
            state.pump_frame(None);
            if predicate(state) {
                return;
            }
        }
    }

    /// 決着がつくまで(または上限`max_frames`まで)1フレームずつ進める。
    fn advance_until_outcome(state: &mut BattleState, max_frames: usize) {
        advance_until(state, max_frames, |state| state.outcome.is_some());
    }

    #[test]
    fn reaching_the_goal_first_wins() {
        // 自分が先にゴール到達しても、#289では全員が結果を出すまで順位は確定しない。
        // 相手が脱落しきった時点で、ゴール到達が脱落より上位という優先順位で1位になる。
        let mut state = battle(1, 2);
        place_just_above_goal(&mut state.games[0]);
        state.games[1].player.lives = 1;
        state.games[1].player.oxygen = 1.0;

        advance_until(&mut state, 10, |state| {
            state.games[0].status == GameStatus::Cleared
        });

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
        assert_eq!(
            state.outcome, None,
            "相手がプレイ中の間は自分の順位も確定しないはず(#289)"
        );
        assert_eq!(state.ranks, vec![None, None]);

        // 酸素切れの後、「天に召される」演出(CRUSH_ASCEND_MS=3000ms)を経てGameOverに
        // なるため、十分なフレーム数(3000msの3倍以上)を回す。
        advance_until_outcome(&mut state, 180);

        assert_eq!(state.games[1].status, GameStatus::GameOver);
        assert_eq!(state.outcome, Some(BattleOutcome::Ranked(1)));
        assert_eq!(
            state.ranks,
            vec![Some(1), Some(2)],
            "ゴール到達者が脱落者より上位になるはず"
        );
    }

    #[test]
    fn the_opponent_dropping_out_first_wins() {
        // 相手が先に脱落(酸素切れ→ライフ0)しても、#289では自分の結果が出るまで決着せず、
        // 後からゴール到達した自分が1位になる。相手の脱落を待つ間に自分が死なないよう、
        // 自分の盤面は無敵にしておく。
        let mut state = battle(3, 4);
        state.games[0].set_invincible(true);
        state.games[1].player.lives = 1;
        state.games[1].player.oxygen = 1.0;

        // 酸素切れの後、「天に召される」演出(CRUSH_ASCEND_MS=3000ms)を経てGameOverに
        // なるため、十分なフレーム数(3000msの3倍以上)を回す。
        advance_until(&mut state, 180, |state| {
            state.games[1].status == GameStatus::GameOver
        });

        assert_eq!(
            state.games[1].status,
            GameStatus::GameOver,
            "前提: 相手が脱落しているはず"
        );
        assert_eq!(
            state.games[0].status,
            GameStatus::Playing,
            "前提: 自分はまだプレイ中のはず"
        );
        assert_eq!(
            state.outcome, None,
            "自分がプレイ中の間は決着しないはず(#289)"
        );

        place_just_above_goal(&mut state.games[0]);
        advance_until_outcome(&mut state, 10);

        assert_eq!(state.games[0].status, GameStatus::Cleared);
        assert_eq!(state.outcome, Some(BattleOutcome::Ranked(1)));
        assert_eq!(state.ranks, vec![Some(1), Some(2)]);
    }

    #[test]
    fn both_reaching_the_goal_at_the_same_time_share_the_first_rank() {
        // 同じ盤面(同じシード)で両者を同じ位置に置くと同じフレームでゴール到達する。
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
        // #289では全員が結果を出すまで決着しないため、相手はゴールさせる。
        let mut state = battle(6, 7);
        state.games[1].set_invincible(true);
        place_just_above_goal(&mut state.games[1]);
        state.games[0].player.lives = 1;
        state.games[0].player.oxygen = 1.0;

        advance_until_outcome(&mut state, 180);

        assert_eq!(state.games[0].status, GameStatus::GameOver);
        assert_eq!(state.games[1].status, GameStatus::Cleared);
        assert_eq!(state.outcome, Some(BattleOutcome::Ranked(2)));
        assert_eq!(state.ranks, vec![Some(2), Some(1)]);
    }

    #[test]
    fn a_long_frame_delta_is_clamped() {
        // 大きく空いたフレームでも、クランプ(`DELTA_CLAMP_MS`)を超えた分は盤面に渡さない。
        // #299で1フレーム=`update`1回になったためフレーム数では長短を見分けられないので、
        // 経過時間に比例して減る酸素で観測する。
        let mut long = battle(10, 11);
        long.advance(Duration::from_secs(10), None);

        let mut clamped = battle(10, 11);
        clamped.advance(Duration::from_millis(DELTA_CLAMP_MS), None);

        let mut shorter = battle(10, 11);
        shorter.advance(Duration::from_millis(DELTA_CLAMP_MS - 50), None);

        assert_eq!(
            long.games[0].player.oxygen, clamped.games[0].player.oxygen,
            "クランプ後の経過時間で進むはず"
        );
        assert_ne!(
            long.games[0].player.oxygen, shorter.games[0].player.oxygen,
            "前提: 渡した経過時間の違いは酸素に出るはず(観測方法の裏取り)"
        );
    }

    #[test]
    fn nothing_advances_after_the_outcome_is_decided() {
        // 決着後は入力を受け付けず、盤面も進めない。#289では全員が結果を出すまで決着
        // しないため、両者をゴールさせて決着を作る(順位そのものはここでは問わない)。
        let mut state = battle(14, 15);
        place_just_above_goal(&mut state.games[0]);
        place_just_above_goal(&mut state.games[1]);
        advance_until_outcome(&mut state, 10);
        assert!(state.outcome.is_some(), "前提: 決着済み");

        let frames_at_outcome = state.games[0].debug_frame();
        state.advance(Duration::from_millis(250), Some(InputAction::MoveRight));

        assert_eq!(
            state.games[0].debug_frame(),
            frames_at_outcome,
            "決着後は盤面が進まないはず"
        );
    }

    #[test]
    fn a_local_input_is_applied_to_my_board_without_any_peer() {
        // #296でローカル専用の経路も対戦画面に出るため、通信路が無くても自分の操作が
        // 自分の盤面へ届くことを見る。#299で見た目専用の先行コピーを廃止したため、
        // 描画が見る`predicted_game()`も同じ盤面を返す。
        let mut state = ai_battle(1);
        let col = state.games[0].player.col;
        open_both_sides(&mut state.games[0]);

        state.advance(Duration::ZERO, Some(InputAction::MoveRight));

        assert_eq!(
            state.games[0].player.col,
            col + 1,
            "自分の操作は自分の盤面へ即座に反映されるはず"
        );
        assert_eq!(
            state.predicted_game().player.col,
            col + 1,
            "描画も同じ盤面を見るはず"
        );
    }

    #[test]
    fn cleared_blocks_are_delivered_to_every_other_player_who_is_still_playing() {
        // #247/#297: 自分が壊したぶんは頭数で割らず、他の参加者へ同じ量を届ける。ただし
        // 受け取る側がすでにゴール・脱落しているなら積まない。通信なしの経路では相手の
        // 状態を直接見られるため、送る側で振り分ける。
        let mut state = battle_n(&[31, 32, 33]);
        state.games[0].set_attack_rules_enabled(true);
        arm_rock_below(&mut state.games[0]);
        state.games[2].status = GameStatus::Cleared;

        state.advance(Duration::ZERO, Some(InputAction::FaceDown));
        state.advance(Duration::ZERO, Some(InputAction::Drill));

        assert!(
            has_incoming_attack(&state.games[1]),
            "プレイ中の参加者には妨害が届くはず"
        );
        assert!(
            !has_incoming_attack(&state.games[2]),
            "ゴール済みの参加者には積まないはず"
        );
    }

    #[test]
    fn new_game_from_battle_config_builds_the_same_board_for_the_same_arguments() {
        // 同一シード・同一設定なら、どの参加者が生成しても初期盤面が完全に一致する
        // (#299でlockstepは廃止したが、全員が同じコースを掘るための前提としてシードの
        // 合意は残る。spec.md 12.2ステップ4)。
        let config = test_battle_config();

        let host_side = new_game_from_battle_config(4242, &config);
        let client_side = new_game_from_battle_config(4242, &config);

        assert_eq!(host_side.board.rows, client_side.board.rows);
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
            base.board.rows,
            new_game_from_battle_config(2, &config).board.rows,
            "シードが違えば盤面も変わるはず"
        );

        let denser_rocks = BattleConfig {
            rock_spawn_rate_percent: 300,
            ..config
        };
        assert_ne!(
            base.board.rows,
            new_game_from_battle_config(1, &denser_rocks).board.rows,
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
        // #289: 1人でもゴール・脱落のどちらにも達していない(Playing/Paused)間は、
        // 誰の順位も決着も出ない。
        assert_eq!(ranks_for(&[Playing, Playing]), (vec![None, None], None));
        assert_eq!(ranks_for(&[Paused, Playing]), (vec![None, None], None));
        assert_eq!(ranks_for(&[Cleared, Playing]), (vec![None, None], None));
        assert_eq!(ranks_for(&[GameOver, Playing]), (vec![None, None], None));
        assert_eq!(ranks_for(&[Playing, Cleared]), (vec![None, None], None));
        assert_eq!(ranks_for(&[Playing, GameOver]), (vec![None, None], None));
        // 両者が同じ結末で、スコアも到達深度も同じなら同順位(旧Draw)。
        assert_eq!(
            ranks_for(&[Cleared, Cleared]),
            (vec![Some(1), Some(1)], Some(Ranked(1)))
        );
        assert_eq!(
            ranks_for(&[GameOver, GameOver]),
            (vec![Some(1), Some(1)], Some(Ranked(1)))
        );
        // ゴール到達はスコア・深度より優先されるため、ゴールした側が必ず上位。
        assert_eq!(
            ranks_for(&[Cleared, GameOver]),
            (vec![Some(1), Some(2)], Some(Ranked(1)))
        );
        assert_eq!(
            ranks_for(&[GameOver, Cleared]),
            (vec![Some(2), Some(1)], Some(Ranked(2)))
        );

        // #289: 同じ結末どうしはスコアの高い方が上位。
        let mut by_score = battle_n(&[1, 1]);
        by_score.games[0].status = Cleared;
        by_score.games[1].status = Cleared;
        by_score.games[1].player.score += 1;
        by_score.update_ranks();
        assert_eq!(by_score.ranks, vec![Some(2), Some(1)]);
        assert_eq!(by_score.outcome, Some(Ranked(2)));

        // #289: スコアも同じなら到達深度の深い方が上位(判定は深度しか見ないため、盤面と
        // 整合しない位置でも行を直接ずらして確かめる)。
        let mut by_depth = battle_n(&[1, 1]);
        by_depth.games[0].status = GameOver;
        by_depth.games[1].status = GameOver;
        by_depth.games[0].player.row += 1;
        by_depth.update_ranks();
        assert_eq!(by_depth.ranks, vec![Some(1), Some(2)]);
        assert_eq!(by_depth.outcome, Some(Ranked(1)));
    }

    #[test]
    fn the_only_player_reaching_the_goal_is_ranked_first_with_three_or_four_players() {
        use BattleOutcome::Ranked;
        use GameStatus::{Cleared, GameOver, Playing};

        // #289: 1人だけがゴール到達した時点では、まだ誰の順位も確定しない。
        assert_eq!(
            ranks_for(&[Cleared, Playing, Playing]),
            (vec![None, None, None], None)
        );
        assert_eq!(
            ranks_for(&[Cleared, Playing, Playing, Playing]),
            (vec![None, None, None, None], None)
        );
        // 全員が結果を出した時点で、唯一のゴール到達者が1位になる(脱落者どうしはスコアも
        // 深度も同じなので同順位)。
        assert_eq!(
            ranks_for(&[Cleared, GameOver, GameOver]),
            (vec![Some(1), Some(2), Some(2)], Some(Ranked(1)))
        );
        assert_eq!(
            ranks_for(&[Cleared, GameOver, GameOver, GameOver]),
            (vec![Some(1), Some(2), Some(2), Some(2)], Some(Ranked(1)))
        );
        // ゴールしたのが自分以外なら、自分は脱落者の側(下位)になる。
        assert_eq!(
            ranks_for(&[GameOver, Cleared, GameOver]),
            (vec![Some(2), Some(1), Some(2)], Some(Ranked(2)))
        );
        assert_eq!(
            ranks_for(&[GameOver, GameOver, Cleared, GameOver]),
            (vec![Some(2), Some(2), Some(1), Some(2)], Some(Ranked(2)))
        );
    }

    #[test]
    fn players_with_the_same_result_share_the_same_rank() {
        use BattleOutcome::Ranked;
        use GameStatus::{Cleared, GameOver};

        // #289: 順位は全員の結果が出てから一括で決まるため、同着は到達の早さではなく
        // 「到達状態・スコア・深度が同じ」ことで生じる。同着で埋まった2つぶんを飛ばして
        // 次の順位が付く。
        assert_eq!(
            ranks_for(&[Cleared, Cleared, GameOver]),
            (vec![Some(1), Some(1), Some(3)], Some(Ranked(1)))
        );
        // 4人で2人がゴール・2人が脱落 → ゴール組が1位、脱落組は3位。
        assert_eq!(
            ranks_for(&[Cleared, GameOver, Cleared, GameOver]),
            (vec![Some(1), Some(3), Some(1), Some(3)], Some(Ranked(1)))
        );
        // 4人全員がゴール → 全員1位。
        assert_eq!(
            ranks_for(&[Cleared, Cleared, Cleared, Cleared]),
            (vec![Some(1); 4], Some(Ranked(1)))
        );
    }

    #[test]
    fn the_last_player_standing_is_ranked_first_after_everyone_else_drops_out() {
        // 4人で自分以外の3人が順番に脱落しても、#289では自分が結果を出すまで誰の順位も
        // 確定しない。最後に自分がゴールすれば1位になる。脱落者どうしの順位は脱落の
        // 先着順ではなくスコアで決まるため、先に脱落した人ほどスコアが低い状況を作る。
        let mut state = battle_n(&[1, 2, 3, 4]);
        state.games[1].player.score = 10;
        state.games[2].player.score = 20;
        state.games[3].player.score = 30;

        state.games[1].status = GameStatus::GameOver;
        state.update_ranks();
        assert_eq!(
            state.ranks,
            vec![None; 4],
            "最初の脱落者の順位もまだ確定しないはず"
        );
        assert_eq!(state.outcome, None, "自分の順位はまだ確定しないはず");

        state.games[2].status = GameStatus::GameOver;
        state.update_ranks();
        assert_eq!(state.ranks, vec![None; 4]);
        assert_eq!(state.outcome, None);

        state.games[3].status = GameStatus::GameOver;
        state.update_ranks();
        assert_eq!(
            state.ranks,
            vec![None; 4],
            "自分がプレイ中の間は、自分1人だけ残っていても確定しないはず"
        );
        assert_eq!(state.outcome, None);

        state.games[0].status = GameStatus::Cleared;
        state.update_ranks();
        assert_eq!(
            state.ranks,
            vec![Some(1), Some(4), Some(3), Some(2)],
            "ゴールした自分が1位で、脱落者はスコアの高い順に並ぶはず"
        );
        assert_eq!(state.outcome, Some(BattleOutcome::Ranked(1)));
    }

    #[test]
    fn a_goal_and_a_dropout_rank_from_both_ends_while_the_survivors_stay_undecided() {
        use GameStatus::{Cleared, GameOver, Playing};

        // 4人で自分がゴール・1人が脱落・残り2人がプレイ中。#289では生存者がいる間は
        // ゴールした自分も脱落者も順位が出ない。
        assert_eq!(
            ranks_for(&[Cleared, Playing, GameOver, Playing]),
            (vec![None, None, None, None], None)
        );
        // 生存者2人も結果を出せば、ゴール組が上・脱落組が下でまとまって確定する。
        assert_eq!(
            ranks_for(&[Cleared, Cleared, GameOver, GameOver]),
            (
                vec![Some(1), Some(1), Some(3), Some(3)],
                Some(BattleOutcome::Ranked(1))
            )
        );
    }

    #[test]
    fn the_outcome_stays_undecided_while_i_am_one_of_the_survivors() {
        use GameStatus::{Cleared, GameOver, Playing};

        // 上と同じ状況で、自分が未確定の2人の一方である場合。誰の順位も出ないため対戦は続く。
        assert_eq!(
            ranks_for(&[Playing, Cleared, GameOver, Playing]),
            (vec![None, None, None, None], None)
        );
        // #289: 自分だけが残った場合も、自分の結果が出るまで確定しない(旧仕様では
        // ここで自動的に順位が付いていた)。
        assert_eq!(
            ranks_for(&[Playing, Cleared, GameOver, GameOver]),
            (vec![None, None, None, None], None)
        );
    }

    #[test]
    fn a_three_player_battle_advances_every_board_and_ranks_the_goal_reacher_first() {
        // `advance`(通信なし)がN人でも全員ぶんの盤面を進め、ゴール到達者を1位にすることを
        // 実際にフレームを回して確認する。#289では全員が結果を出すまで決着しないため、自分
        // 以外の2人は脱落させる(酸素切れ→ライフ0)。
        let mut state = battle_n(&[21, 22, 23]);
        place_just_above_goal(&mut state.games[0]);
        for game in state.games[1..].iter_mut() {
            game.player.lives = 1;
            game.player.oxygen = 1.0;
        }

        advance_until_outcome(&mut state, 200);

        assert_eq!(state.games[0].status, GameStatus::Cleared);
        assert!(
            state.games[1..]
                .iter()
                .all(|game| game.status == GameStatus::GameOver),
            "前提: 自分以外は脱落しているはず"
        );
        assert!(
            state.games[1..].iter().all(|game| game.debug_frame() > 0),
            "自分以外の盤面もフレームぶん進んでいるはず"
        );
        assert_eq!(state.outcome, Some(BattleOutcome::Ranked(1)));
        assert_eq!(state.ranks[0], Some(1), "ゴール到達者が1位のはず");
        assert!(
            state.ranks[1..]
                .iter()
                .all(|rank| rank.is_some_and(|rank| rank >= 2)),
            "脱落した2人はゴール到達者より下位のはず(2人のスコア差次第で2位/3位が入れ替わる)"
        );
    }

    // -----------------------------------------------------------------------
    // 通信あり(#254)。ループバックTCPで#253のハンドシェイクを実際に行ってから、
    // 2つの`BattleState`をそれぞれの実時間で進める(#299で待ち合わせは無くなった)。
    // -----------------------------------------------------------------------

    /// テストのポンプ回数の上限。ループバックの配送待ちで何周か空回りするため、必要な
    /// フレーム数より多めに取る。ここまで回して条件が揃わなければ実装の不具合とみなす。
    const MAX_PUMPS: usize = 500;

    /// 1周ごとに挟む待ち時間。受信スレッドがメッセージを届ける隙を作るためのもので、
    /// これが無いとポンプの空回りだけで上限に達し、相手の入力が届く前に打ち切られる。
    fn pump_interval() {
        thread::sleep(Duration::from_millis(1));
    }

    /// 通信ありの状態が持つ`PeerLink`のうち`index`番目(`games[index + 1]`に対応)を
    /// 取り出す。
    fn link_at(state: &BattleState, index: usize) -> &PeerLink {
        state.peers.as_ref().expect("通信ありの対戦状態のはず")[index]
            .as_ref()
            .expect("AIの枠(#300)ではなく人間との接続のはず")
    }

    /// 2人対戦で唯一の`PeerLink`を取り出す(#254のテスト群用)。
    fn link(state: &BattleState) -> &PeerLink {
        link_at(state, 0)
    }

    /// ループバックTCPで#253のハンドシェイクを実行し、ホスト側・クライアント側の
    /// `BattleState`を返す。
    fn connected_pair() -> (BattleState, BattleState) {
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

        (
            BattleState::from_handshake(host_result, host_stream, "host").unwrap(),
            BattleState::from_handshake(client_result, client_stream, "client").unwrap(),
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

    /// 生ストリーム側で、`predicate`に合うメッセージが届くまで読み進める。Heartbeatや
    /// 自分に関係のないメッセージが先に届くため、種別で選り分ける必要がある。
    fn read_message_matching(
        peer: &mut TcpStream,
        predicate: impl Fn(&GameMessage) -> bool,
    ) -> GameMessage {
        loop {
            let message = net::read_message(peer).expect("目的のメッセージが届くはず");
            if predicate(&message) {
                return message;
            }
        }
    }

    #[test]
    fn the_first_frame_of_a_battle_interrupts_neither_side() {
        // #299のリグレッション。lockstepではハンドシェイク直後の1フレーム目に状態の
        // 食い違いを検出して対戦を打ち切ることがあった。非同期方式では盤面の突き合わせを
        // 行わないため、誰も操作していない1フレーム目から双方が普通に進む。
        let (mut host, mut client) = connected_pair();

        host.pump_frame(None);
        client.pump_frame(None);

        for (side, state) in [("ホスト", &host), ("クライアント", &client)] {
            assert_eq!(
                state.outcome, None,
                "{side}: 1フレーム目で対戦が終わってはいけない"
            );
            assert!(
                state.games.iter().all(|game| game.debug_frame() > 0),
                "{side}: 全員ぶんの盤面が1フレーム進むはず"
            );
            assert!(
                state
                    .games
                    .iter()
                    .all(|game| game.status == GameStatus::Playing),
                "{side}: まだ全員プレイ中のはず"
            );
            assert!(
                !link(state).disconnected,
                "{side}: 相手が切断扱いになってはいけない"
            );
        }

        // その後も続けて進める。何フレーム経っても中断しない。
        for _ in 0..20 {
            host.pump_frame(None);
            client.pump_frame(None);
            pump_interval();
        }

        for (side, state) in [("ホスト", &host), ("クライアント", &client)] {
            assert_eq!(state.outcome, None, "{side}: 対戦が中断してはいけない");
            assert!(
                !link(state).disconnected,
                "{side}: 切断扱いになってはいけない"
            );
        }
    }

    #[test]
    fn inputs_are_exchanged_between_two_connected_battle_states() {
        // #299: 受け取った操作は順番待ちをせず、届いた瞬間にその参加者の盤面へ反映する。
        // 双方が逆方向へ動き、互いの手元にある相手のコピーがその通りに動くことを見る。
        let (mut host, mut client) = connected_pair();
        for game in host.games.iter_mut().chain(client.games.iter_mut()) {
            open_both_sides(game);
        }
        let col = host.games[0].player.col;

        host.advance(Duration::ZERO, Some(InputAction::MoveRight));
        client.advance(Duration::ZERO, Some(InputAction::MoveLeft));
        assert_eq!(
            host.games[0].player.col,
            col + 1,
            "前提: 自分の操作は自分の盤面へ即反映されるはず"
        );
        assert_eq!(client.games[0].player.col, col - 1, "前提: 同じ");

        for _ in 0..MAX_PUMPS {
            // 盤面を進めずに受信だけ回す(配送待ちで酸素を消費させない)。
            host.advance(Duration::ZERO, None);
            client.advance(Duration::ZERO, None);
            if host.games[1].player.col == col - 1 && client.games[1].player.col == col + 1 {
                break;
            }
            pump_interval();
        }

        assert_eq!(
            host.games[1].player.col,
            col - 1,
            "クライアントのMoveLeftがホスト側の相手盤面へ届くはず"
        );
        assert_eq!(
            client.games[1].player.col,
            col + 1,
            "ホストのMoveRightがクライアント側の相手盤面へ届くはず"
        );
        assert_eq!(host.outcome, None, "この範囲では決着しないはず");
        assert_eq!(client.outcome, None);
    }

    #[test]
    fn an_incoming_attack_is_applied_only_while_my_own_board_is_playing() {
        // #247/#297: 妨害岩を適用するかは、送った側ではなく受け取った側の状態で決める
        // (spec.md 12.8)。相手役は自分の状態を問わず同じ`Attack`を送ってくる。
        for (label, my_status, expected) in [
            ("プレイ中", GameStatus::Playing, true),
            ("ゴール済み", GameStatus::Cleared, false),
        ] {
            let (mut host, mut peer) = battle_with_raw_peer();
            host.games[0].status = my_status;

            // Attackに続けてByeを送る。TCPは順序を保ち、受信は届いたぶんを1周で読み切る
            // ため、切断扱いになった時点でAttackも処理済みとみなせる。
            net::write_message(
                &mut peer,
                &GameMessage::Attack {
                    amount: 8,
                    proxy_for: None,
                },
            )
            .unwrap();
            net::write_message(&mut peer, &GameMessage::Bye).unwrap();

            for _ in 0..MAX_PUMPS {
                host.advance(Duration::ZERO, None);
                if link(&host).disconnected {
                    break;
                }
                pump_interval();
            }

            assert!(
                link(&host).disconnected,
                "{label}: 前提: Attackに続くByeまで処理されるはず"
            );
            assert_eq!(
                has_incoming_attack(&host.games[0]),
                expected,
                "{label}: 受け取った側の状態で適用するかどうかが決まるはず"
            );
        }
    }

    #[test]
    fn a_result_from_the_opponent_settles_my_copy_of_their_board() {
        // 12.4: 盤面の完全一致を前提にしないため、相手が申告してきた結末を自分が持つ
        // 相手のコピーへ反映する。これが無いと自分の手元で相手のコピーが延々とプレイ中の
        // まま残り、全員の結果がそろわず順位が確定しない。
        for (label, reached_goal, expected_status) in [
            ("ゴール", true, GameStatus::Cleared),
            ("脱落", false, GameStatus::GameOver),
        ] {
            let (mut host, mut peer) = battle_with_raw_peer();
            place_just_above_goal(&mut host.games[0]);
            advance_until(&mut host, 20, |host| {
                host.games[0].status == GameStatus::Cleared
            });
            assert_eq!(
                host.games[0].status,
                GameStatus::Cleared,
                "{label}: 前提: 自分はゴールしているはず"
            );
            assert_eq!(
                host.outcome, None,
                "{label}: 前提: 相手の結果が出るまでは決着しないはず"
            );

            net::write_message(
                &mut peer,
                &GameMessage::Result {
                    reached_goal,
                    time_ms: 0,
                    proxy_for: None,
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
                host.games[1].status, expected_status,
                "{label}: 申告した結末が自分の持つコピーへ反映されるはず"
            );
            assert!(
                host.outcome.is_some(),
                "{label}: 全員の結果がそろえば順位が確定するはず"
            );
        }
    }

    #[test]
    fn receiving_bye_from_the_opponent_ends_the_battle_as_a_win_by_default() {
        let (mut host, mut peer) = battle_with_raw_peer();
        net::write_message(&mut peer, &GameMessage::Bye).unwrap();

        // `delta`を0にして回すと盤面が進まないため、Heartbeatの途絶ではなくBye受信だけで
        // 相手が脱落することを確認できる。
        for _ in 0..MAX_PUMPS {
            host.advance(Duration::ZERO, None);
            if link(&host).disconnected {
                break;
            }
            pump_interval();
        }

        assert!(link(&host).disconnected);
        assert_eq!(host.games[1].status, GameStatus::GameOver);
        assert_eq!(
            host.outcome, None,
            "#289: 自分の結果が出るまでは不戦勝も確定しないはず"
        );

        // 自分がゴールすれば全員の結果が揃い、Byeで抜けた相手より上位(1位)で決着する。
        place_just_above_goal(&mut host.games[0]);
        advance_until_outcome(&mut host, 20);

        assert_eq!(host.games[0].status, GameStatus::Cleared);
        assert_eq!(host.outcome, Some(BattleOutcome::Ranked(1)));
        assert_eq!(host.ranks, vec![Some(1), Some(2)]);
    }

    #[test]
    fn a_heartbeat_is_sent_while_no_input_happens() {
        // 誰も操作していない間は、生存を示すのがHeartbeatだけになる(12.4)。
        let (mut host, mut peer) = battle_with_raw_peer();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(HEARTBEAT_INTERVAL_MS + 200) {
            // 操作を渡さないため、Inputは1件も送られない。
            host.advance(Duration::ZERO, None);
            thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(
            net::read_message(&mut peer).unwrap(),
            GameMessage::Heartbeat
        );
    }

    #[test]
    fn silence_longer_than_the_heartbeat_timeout_counts_as_a_disconnect() {
        // 相手役はInputもHeartbeatも送らない。何も届かない状態が上限を超えた時点で
        // 切断と判定される。
        let (mut host, _peer) = battle_with_raw_peer();

        let started = Instant::now();
        let deadline = Duration::from_millis(HEARTBEAT_TIMEOUT_MS * 2);
        while !link(&host).disconnected && started.elapsed() < deadline {
            host.advance(Duration::ZERO, None);
            thread::sleep(Duration::from_millis(10));
        }

        assert!(link(&host).disconnected, "切断扱いになるはず");
        assert!(
            started.elapsed() >= Duration::from_millis(HEARTBEAT_TIMEOUT_MS),
            "途絶の上限に達する前に切断扱いにはしないはず"
        );
        assert_eq!(host.games[1].status, GameStatus::GameOver);
        assert_eq!(
            host.outcome, None,
            "#289: 切断で相手が脱落しても、自分の結果が出るまでは決着しないはず"
        );

        // 自分がゴールすれば全員の結果が揃い、切断した相手より上位(1位)で決着する。
        place_just_above_goal(&mut host.games[0]);
        advance_until_outcome(&mut host, 20);

        assert_eq!(host.games[0].status, GameStatus::Cleared);
        assert_eq!(host.outcome, Some(BattleOutcome::Ranked(1)));
        assert_eq!(host.ranks, vec![Some(1), Some(2)]);
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
            let streams: Vec<Option<TcpStream>> = order[1..]
                .iter()
                .map(|&p| Some(row[p].take().expect("各ペアに1本ずつ用意している")))
                .collect();
            states.push(
                BattleState::from_peer_streams(
                    games,
                    player_names,
                    streams,
                    // 参加者番号とルーム内インデックスを同じ並びで扱う。
                    h,
                    start_at_unix_ms,
                )
                .unwrap(),
            );
        }
        states
    }

    /// 参加者0だけ`BattleState`を作り、他の参加者は生のTCPストリームのままにする。
    /// 送信内容を直接覗くテスト用(`battle_with_raw_peer`のN人版)。
    /// 戻り値の`peers[i]`は`games[i+1]`に対応する。
    fn battle_with_raw_peers(n: usize, seed: u64) -> (BattleState, Vec<TcpStream>) {
        let config = test_battle_config();
        let mut host_streams = Vec::with_capacity(n - 1);
        let mut raw_peers = Vec::with_capacity(n - 1);
        for _ in 1..n {
            let (mine, theirs) = loopback_pair();
            host_streams.push(Some(mine));
            raw_peers.push(theirs);
        }

        let games: Vec<Game> = (0..n)
            .map(|_| new_game_from_battle_config(seed, &config))
            .collect();
        let player_names: Vec<String> = (0..n).map(|p| format!("p{p}")).collect();
        let state = BattleState::from_peer_streams(
            games,
            player_names,
            host_streams,
            0,
            net::unix_time_ms(),
        )
        .unwrap();
        (state, raw_peers)
    }

    /// 全員を`frames`フレームぶん進める。1フレームごとに待ち時間を挟み、互いの送信が
    /// 相手へ届く隙を作る。
    fn pump_all_frames(states: &mut [BattleState], frames: usize) {
        for _ in 0..frames {
            for state in states.iter_mut() {
                state.pump_frame(None);
            }
            pump_interval();
        }
    }

    /// 全員の決着が出るまでポンプする。決着後はフレームが進まないため、フレーム数ではなく
    /// 決着で打ち切る必要がある(#289)。
    fn pump_all_until_outcome(states: &mut [BattleState]) {
        for _ in 0..MAX_PUMPS {
            for state in states.iter_mut() {
                state.pump_frame(None);
            }
            if states.iter().all(|state| state.outcome.is_some()) {
                return;
            }
            pump_interval();
        }
    }

    #[test]
    fn cleared_blocks_are_broadcast_to_every_peer_without_being_divided() {
        // #247/#297: 3人以上でも、自分が壊したぶんを頭数で割らず同じ量を全員へ送る
        // (N人時の配分ルールはユーザー確認済み)。
        const N: usize = 3;
        let (mut host, mut peers) = battle_with_raw_peers(N, 9301);
        for peer in peers.iter_mut() {
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        arm_rock_below(&mut host.games[0]);

        host.advance(Duration::ZERO, Some(InputAction::FaceDown));
        host.advance(Duration::ZERO, Some(InputAction::Drill));

        for (index, peer) in peers.iter_mut().enumerate() {
            assert_eq!(
                read_message_matching(peer, |message| matches!(
                    message,
                    GameMessage::Attack { .. }
                )),
                GameMessage::Attack {
                    amount: 1,
                    proxy_for: None,
                },
                "peer{index}: 壊した1ブロックぶんがそのまま届くはず"
            );
        }
    }

    /// `n`人のフルメッシュで、全員の操作が他の全員の手元へ届くことを確認する。参加者番号の
    /// 偶奇で動く向きを分け、視点ごとの並び替え(`games_index_of`)も含めて検証する。
    fn assert_full_mesh_inputs_reach_everyone(n: usize, seed: u64) {
        let mut states = connected_mesh(n, seed);
        for state in states.iter_mut() {
            for game in state.games.iter_mut() {
                open_both_sides(game);
            }
        }
        let col = states[0].games[0].player.col;
        // 参加者`p`が動いた後の列。偶数番は右、奇数番は左へ1マス動く。
        let target_col = |p: usize| {
            if p.is_multiple_of(2) {
                col + 1
            } else {
                col - 1
            }
        };

        for (p, state) in states.iter_mut().enumerate() {
            let action = if p.is_multiple_of(2) {
                InputAction::MoveRight
            } else {
                InputAction::MoveLeft
            };
            state.advance(Duration::ZERO, Some(action));
        }

        for _ in 0..MAX_PUMPS {
            for state in states.iter_mut() {
                // 盤面を進めずに受信だけ回す(配送待ちで酸素を消費させない)。
                state.advance(Duration::ZERO, None);
            }
            let all_arrived = states.iter().enumerate().all(|(h, state)| {
                (0..n).all(|p| state.games[games_index_of(n, h, p)].player.col == target_col(p))
            });
            if all_arrived {
                break;
            }
            pump_interval();
        }

        for (h, state) in states.iter().enumerate() {
            assert!(
                state
                    .peers
                    .as_ref()
                    .is_some_and(|peers| peers.len() == n - 1),
                "参加者{h}: 自分以外の全員と接続を持つはず"
            );
            assert_eq!(state.outcome, None, "参加者{h}: この範囲では決着しないはず");
            for p in 0..n {
                assert_eq!(
                    state.games[games_index_of(n, h, p)].player.col,
                    target_col(p),
                    "参加者{h}の視点で参加者{p}の操作が反映されていない"
                );
            }
        }
    }

    #[test]
    fn three_participants_in_a_full_mesh_exchange_inputs() {
        assert_full_mesh_inputs_reach_everyone(3, 9201);
    }

    #[test]
    fn four_participants_in_a_full_mesh_exchange_inputs() {
        assert_full_mesh_inputs_reach_everyone(4, 9202);
    }

    #[test]
    fn a_participant_leaving_with_bye_is_ranked_last_while_the_others_keep_playing() {
        // 4人のうち1人がByeを送って抜けても、残り3人だけで対戦が進む。#289では全員の結果が
        // 揃うまで順位が出ないため、抜けた人が最下位になるのは残り3人がゴールした後。
        const N: usize = 4;
        const LEAVER: usize = N - 1;

        let mut states = connected_mesh(N, 9203);
        pump_all_frames(&mut states, 3);

        // 抜ける側はByeを送ってから状態を捨てる(接続も閉じる)。
        let mut leaver = states.pop().expect("4人ぶんあるはず");
        leaver.notify_bye();
        drop(leaver);

        for _ in 0..MAX_PUMPS {
            pump_all_frames(&mut states, 1);
            let all_noticed = states
                .iter()
                .enumerate()
                .all(|(h, state)| link_at(state, games_index_of(N, h, LEAVER) - 1).disconnected);
            if all_noticed {
                break;
            }
        }

        for (h, state) in states.iter().enumerate() {
            let leaver_index = games_index_of(N, h, LEAVER);
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
                state.ranks[leaver_index], None,
                "参加者{h}: #289では生存者がいる間は抜けた参加者の順位も出ないはず"
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

        // 残った3人がゴールすると全員の結果が揃い、抜けた参加者が最下位で確定する。
        for (h, state) in states.iter_mut().enumerate() {
            let leaver_index = games_index_of(N, h, LEAVER);
            for (index, game) in state.games.iter_mut().enumerate() {
                if index != leaver_index {
                    place_just_above_goal(game);
                }
            }
        }
        pump_all_until_outcome(&mut states);

        for (h, state) in states.iter().enumerate() {
            let leaver_index = games_index_of(N, h, LEAVER);
            assert_eq!(
                state.games[0].status,
                GameStatus::Cleared,
                "参加者{h}: 前提: 自分はゴールしているはず"
            );
            assert_eq!(
                state.ranks[leaver_index],
                Some(N as u8),
                "参加者{h}: 抜けた参加者は最下位で確定するはず"
            );
            assert_eq!(
                state.outcome,
                Some(BattleOutcome::Ranked(1)),
                "参加者{h}: ゴールした3人は同条件なので全員1位のはず"
            );
        }
    }

    #[test]
    fn a_participant_that_goes_silent_is_ranked_last_while_the_others_keep_playing() {
        // 1人がInputもHeartbeatも送らなくなった場合も同様に、途絶の上限
        // (`HEARTBEAT_TIMEOUT_MS`)を超えた時点で脱落扱いになり、残りの参加者で対戦を
        // 続ける。状態は束縛したままにして接続自体は生かし、Byeではなく途絶で脱落する
        // ことを見る。
        const N: usize = 4;
        const SILENT: usize = N - 1;

        let mut states = connected_mesh(N, 9204);
        pump_all_frames(&mut states, 2);
        let _silent = states.pop().expect("4人ぶんあるはず");

        let started = Instant::now();
        let deadline = Duration::from_millis(HEARTBEAT_TIMEOUT_MS * 3);
        while started.elapsed() < deadline {
            for state in states.iter_mut() {
                // 盤面を進めずに受信・切断判定だけ回す(待つ間に酸素を消費させない)。
                state.advance(Duration::ZERO, None);
            }
            let all_noticed = states
                .iter()
                .enumerate()
                .all(|(h, state)| link_at(state, games_index_of(N, h, SILENT) - 1).disconnected);
            if all_noticed {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        // 「上限に達する前に切断扱いにしない」ことは2人の
        // `silence_longer_than_the_heartbeat_timeout_counts_as_a_disconnect`で見ている
        // (ここでは`started`がメッシュ確立の後になるため、下限の時間を測る基準にできない)。
        for (h, state) in states.iter().enumerate() {
            let silent_index = games_index_of(N, h, SILENT);
            assert!(
                link_at(state, silent_index - 1).disconnected,
                "参加者{h}: 送信の途絶えた参加者は切断扱いになるはず"
            );
            assert_eq!(
                state.games[silent_index].status,
                GameStatus::GameOver,
                "参加者{h}: 順位確定のため途絶えた参加者の盤面はGameOverへ倒すはず"
            );
            assert_eq!(
                state.ranks[silent_index], None,
                "参加者{h}: #289では生存者がいる間は脱落者の順位も出ないはず"
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

        // 残った3人がゴールすると全員の結果が揃い、途絶えた参加者が最下位で確定する。
        for (h, state) in states.iter_mut().enumerate() {
            let silent_index = games_index_of(N, h, SILENT);
            for (index, game) in state.games.iter_mut().enumerate() {
                if index != silent_index {
                    place_just_above_goal(game);
                }
            }
        }
        pump_all_until_outcome(&mut states);

        for (h, state) in states.iter().enumerate() {
            let silent_index = games_index_of(N, h, SILENT);
            assert_eq!(
                state.games[0].status,
                GameStatus::Cleared,
                "参加者{h}: 前提: 自分はゴールしているはず"
            );
            assert_eq!(
                state.ranks[silent_index],
                Some(N as u8),
                "参加者{h}: 途絶えた参加者は最下位で確定するはず"
            );
            assert_eq!(
                state.outcome,
                Some(BattleOutcome::Ranked(1)),
                "参加者{h}: ゴールした3人は同条件なので全員1位のはず"
            );
        }
    }

    // -----------------------------------------------------------------------
    // ローカルAI対戦(#296)。通信を一切使わず、相手をオートプレイ(#221)が操作する。
    // -----------------------------------------------------------------------

    /// テスト用のローカルAI対戦(#296)。AI`ai_count`人を相手にする(合計`ai_count + 1`人)。
    /// 人間ぶんもAIぶんも`new_game_from_battle_config`で同じシード・設定から作る
    /// (実際のロビーからの入口と同じ作り方にするため)。
    fn ai_battle(ai_count: usize) -> BattleState {
        let config = test_battle_config();
        let seed = 7;
        let human_game = new_game_from_battle_config(seed, &config);
        let ai_games = (0..ai_count)
            .map(|_| new_game_from_battle_config(seed, &config))
            .collect();
        BattleState::new_local_vs_ai(human_game, ai_games, "me".to_string())
    }

    #[test]
    fn the_ai_opponents_drill_down_on_their_own() {
        // #296: AIは誰からも入力をもらわずに自力で掘り進む(オートプレイの判断を
        // 毎フレーム1回ぶん適用する)。
        let mut state = ai_battle(2);
        let start_rows: Vec<usize> = state.games[1..].iter().map(|g| g.player.row).collect();

        advance_until(&mut state, 200, |state| {
            state.games[1..]
                .iter()
                .zip(&start_rows)
                .all(|(game, &row)| game.player.row > row)
        });

        for (index, &start_row) in start_rows.iter().enumerate() {
            assert!(
                state.games[index + 1].player.row > start_row,
                "AI {}は自力で下へ進むはず(開始{}行目のまま止まっている)",
                index + 1,
                start_row
            );
        }
    }

    #[test]
    fn an_ai_battle_never_uses_the_network() {
        // #296: AI対戦は通信を一切使わない。進めても通信路は生えないし、退出時の
        // Bye送信(送る相手がいない)でも落ちない。
        let mut state = ai_battle(1);
        assert!(state.peers.is_none(), "前提: 通信路を持たないはず");

        for _ in 0..10 {
            state.pump_frame(None);
        }

        assert!(state.peers.is_none(), "AI対戦中に通信路が生えてはいけない");
        state.notify_bye();
    }

    #[test]
    fn the_ai_opponents_are_named_in_order() {
        // #296: 表示名はindex 0が自分で、AIは1から順に振る。
        let state = ai_battle(3);

        assert_eq!(state.games.len(), 4, "自分+AI3人ぶんの盤面があるはず");
        assert_eq!(
            state.player_names,
            vec![
                "me".to_string(),
                "AI 1".to_string(),
                "AI 2".to_string(),
                "AI 3".to_string(),
            ]
        );
    }

    #[test]
    fn an_ai_opponent_can_finish_the_course_by_itself() {
        // #296: AIは掘り進むだけでなく、誰の手も借りずにゴールまで到達できる
        // (オートプレイ(#221)をそのまま相手として使えていることの確認)。
        let mut state = ai_battle(1);

        advance_until(&mut state, 300, |state| {
            state.games[1].status != GameStatus::Playing
        });

        assert_eq!(
            state.games[1].status,
            GameStatus::Cleared,
            "AIは自力でゴールできるはず(到達行={} / ゴール行={})",
            state.games[1].player.row,
            TEST_GOAL_M - 1
        );
    }

    // -----------------------------------------------------------------------
    // ルームへ混ぜたAI(#300)。AIはホストの手元でだけ動き、ホストがその入力・妨害岩・
    // 結果を全員へ代理送信する。ゲストから見ると「接続を持たない追加の参加者」になる。
    // -----------------------------------------------------------------------

    /// room内インデックス`p`の表示名。人間は`p0`,`p1`,...とし、AIはルームが付ける名前
    /// (`room::ai_member_name`)に合わせる。ルームの並びと同じくAIは人間の後ろに来る。
    fn room_member_name(human_count: usize, p: usize) -> String {
        if p < human_count {
            format!("p{p}")
        } else {
            crate::room::ai_member_name(p - human_count + 1)
        }
    }

    /// 人間`human_count`人+AI`ai_count`人のルームで、参加者`my_index`(人間)の
    /// `BattleState`を作る。AIはroom内インデックスの末尾に並ぶ(#300)。
    ///
    /// 他の人間ぶんは生のTCPストリームで返し、代理送信の中身を覗いたり偽のメッセージを
    /// 送りつけたりできるようにする。AIは人間より後ろに並ぶため、`raw_peers[i]`は
    /// `games[i + 1]`にいる人間に対応する。
    fn battle_with_ai_slots(
        human_count: usize,
        ai_count: usize,
        my_index: usize,
        seed: u64,
    ) -> (BattleState, Vec<TcpStream>) {
        let config = test_battle_config();
        let total = human_count + ai_count;
        // `games`の並び(index 0が自分、残りはroom内インデックス順)。
        let order: Vec<usize> = std::iter::once(my_index)
            .chain((0..total).filter(|&p| p != my_index))
            .collect();

        let mut streams = Vec::with_capacity(total - 1);
        let mut raw_peers = Vec::new();
        for &p in &order[1..] {
            if p < human_count {
                let (mine, theirs) = loopback_pair();
                streams.push(Some(mine));
                raw_peers.push(theirs);
            } else {
                // AIの枠は接続を持たない(#300)。
                streams.push(None);
            }
        }

        let games: Vec<Game> = order
            .iter()
            .map(|_| new_game_from_battle_config(seed, &config))
            .collect();
        let player_names: Vec<String> = order
            .iter()
            .map(|&p| room_member_name(human_count, p))
            .collect();
        let state = BattleState::from_peer_streams(
            games,
            player_names,
            streams,
            my_index,
            net::unix_time_ms(),
        )
        .unwrap();
        (state, raw_peers)
    }

    /// 人間`human_count`人+AI`ai_count`人のルームを、人間どうしのフルメッシュ接続込みで
    /// 組む(`connected_mesh`のAI混在版)。戻り値のindexはroom内インデックスで、人間ぶん
    /// (`0..human_count`)だけが並ぶ。
    fn connected_mesh_with_ai(human_count: usize, ai_count: usize, seed: u64) -> Vec<BattleState> {
        let config = test_battle_config();
        let total = human_count + ai_count;
        // `sockets[a][b]`=参加者aから参加者bへ向かう接続。AIは接続を持たないため
        // 人間ぶんだけ張る(#300)。
        let mut sockets: Vec<Vec<Option<TcpStream>>> = (0..human_count)
            .map(|_| (0..human_count).map(|_| None).collect())
            .collect();
        for (a, b) in (0..human_count).flat_map(|a| ((a + 1)..human_count).map(move |b| (a, b))) {
            let (to_b, to_a) = loopback_pair();
            sockets[a][b] = Some(to_b);
            sockets[b][a] = Some(to_a);
        }

        // 開始時刻はハンドシェイク(#275)で合意する値の代わり。
        let start_at_unix_ms = net::unix_time_ms();
        let mut states = Vec::with_capacity(human_count);
        for (h, mut row) in sockets.into_iter().enumerate() {
            let order = participant_order(total, h);
            let games: Vec<Game> = order
                .iter()
                .map(|_| new_game_from_battle_config(seed, &config))
                .collect();
            let player_names: Vec<String> = order
                .iter()
                .map(|&p| room_member_name(human_count, p))
                .collect();
            let streams: Vec<Option<TcpStream>> = order[1..]
                .iter()
                .map(|&p| {
                    (p < human_count).then(|| row[p].take().expect("各ペアに1本ずつ用意している"))
                })
                .collect();
            states.push(
                BattleState::from_peer_streams(games, player_names, streams, h, start_at_unix_ms)
                    .unwrap(),
            );
        }
        states
    }

    /// 盤面とプレイヤーの状態が一致しているか。同じ初期盤面へ同じ操作が同じ順で適用された
    /// かどうかを突き合わせるために使う。
    fn same_board_and_player(a: &Game, b: &Game) -> bool {
        a.player.row == b.player.row
            && a.player.col == b.player.col
            && a.player.facing == b.player.facing
            && a.board.rows == b.board.rows
    }

    #[test]
    fn room_indexes_and_games_indexes_convert_back_and_forth_for_everyone() {
        // #300: 代理送信はroom内インデックスで適用先を指すため、受け取った側は自分の
        // `games`の並びへ変換する必要がある。視点(`my_index`)ごとにずれ方が変わるので、
        // 全員ぶんの視点で往復が一致することを確認する。
        const HUMANS: usize = 3;
        const AIS: usize = 1;

        for my_index in 0..HUMANS {
            let (state, _peers) = battle_with_ai_slots(HUMANS, AIS, my_index, 30001);
            assert_eq!(
                state.games.len(),
                HUMANS + AIS,
                "my_index={my_index}: AIぶんも含めた全員の盤面を持つはず"
            );
            assert_eq!(
                state.games_index_for_room_index(my_index),
                0,
                "my_index={my_index}: 自分はいつでもgames[0]のはず"
            );

            for games_index in 0..state.games.len() {
                let room_index = state.room_index_for_games_index(games_index);
                assert_eq!(
                    state.games_index_for_room_index(room_index),
                    games_index,
                    "my_index={my_index}: games[{games_index}]の変換が往復しない"
                );
            }

            for room_index in 0..(HUMANS + AIS) {
                // 自分より前にいる参加者は自分を飛ばすぶん1つ後ろへずれ、後ろにいる
                // 参加者はそのままの位置に来る。
                let expected = if room_index == my_index {
                    0
                } else if room_index < my_index {
                    room_index + 1
                } else {
                    room_index
                };
                assert_eq!(
                    state.games_index_for_room_index(room_index),
                    expected,
                    "my_index={my_index}: room内インデックス{room_index}の変換先が違う"
                );
            }
        }
    }

    #[test]
    fn a_proxied_input_lands_on_the_ai_slot_instead_of_the_sender() {
        // #300: ホストはAIの入力を`proxy_for`付きで送る。受け取った側が変換を誤ると、
        // 送ってきたホストの盤面が動いてしまう(AIの盤面は止まったまま残る)。
        const HUMANS: usize = 2;
        const AIS: usize = 1;
        const AI_ROOM_INDEX: usize = HUMANS;
        // ゲスト(room内インデックス1)の視点。games[1]がホスト、games[2]がAIになる。
        let (mut guest, mut peers) = battle_with_ai_slots(HUMANS, AIS, 1, 30002);
        assert_eq!(
            guest.games_index_for_room_index(AI_ROOM_INDEX),
            2,
            "前提: AIはgames[2]にいるはず"
        );
        for game in guest.games.iter_mut() {
            open_both_sides(game);
        }
        let col = guest.games[0].player.col;

        net::write_message(
            &mut peers[0],
            &GameMessage::Input {
                action: NetAction::MoveRight,
                proxy_for: Some(AI_ROOM_INDEX),
            },
        )
        .unwrap();

        for _ in 0..MAX_PUMPS {
            // 盤面を進めずに受信だけ回す(配送待ちで酸素を消費させない)。
            guest.advance(Duration::ZERO, None);
            if guest.games[2].player.col == col + 1 {
                break;
            }
            pump_interval();
        }

        assert_eq!(
            guest.games[2].player.col,
            col + 1,
            "代理送信された入力はAIの枠へ届くはず"
        );
        assert_eq!(
            guest.games[1].player.col, col,
            "送ってきたホストの盤面は動かないはず"
        );
    }

    #[test]
    fn a_proxied_result_settles_the_ai_slot_instead_of_the_sender() {
        // #300: AIの結末もホストが代理送信する。これが無いとゲスト側でAIの盤面が
        // プレイ中のまま残り、全員の結果がそろわず順位が確定しない(12.4と同じ理屈)。
        const HUMANS: usize = 2;
        const AIS: usize = 1;
        const AI_ROOM_INDEX: usize = HUMANS;
        let (mut guest, mut peers) = battle_with_ai_slots(HUMANS, AIS, 1, 30003);

        net::write_message(
            &mut peers[0],
            &GameMessage::Result {
                reached_goal: true,
                time_ms: 1_234,
                proxy_for: Some(AI_ROOM_INDEX),
            },
        )
        .unwrap();

        for _ in 0..MAX_PUMPS {
            guest.advance(Duration::ZERO, None);
            if guest.games[2].status != GameStatus::Playing {
                break;
            }
            pump_interval();
        }

        assert_eq!(
            guest.games[2].status,
            GameStatus::Cleared,
            "代理送信された結果はAIの枠へ反映されるはず"
        );
        assert_eq!(
            guest.games[1].status,
            GameStatus::Playing,
            "送ってきたホストの盤面は決着していないはず"
        );
    }

    #[test]
    fn a_proxied_attack_is_piled_on_my_own_board_like_any_other_attack() {
        // #300: 妨害岩は「受け取った側が自分の盤面へ積む」ルール(spec.md 12.8)なので、
        // 誰の代理送信かで積む先は変わらない。`proxy_for`が付いていても、自分が
        // プレイ中なら自分の盤面へ届く。
        const HUMANS: usize = 2;
        const AIS: usize = 1;
        let (mut guest, mut peers) = battle_with_ai_slots(HUMANS, AIS, 1, 30004);
        assert!(
            !has_incoming_attack(&guest.games[0]),
            "前提: まだ妨害を受けていないはず"
        );

        net::write_message(
            &mut peers[0],
            &GameMessage::Attack {
                amount: 8,
                proxy_for: Some(HUMANS),
            },
        )
        .unwrap();

        for _ in 0..MAX_PUMPS {
            guest.advance(Duration::ZERO, None);
            if has_incoming_attack(&guest.games[0]) {
                break;
            }
            pump_interval();
        }

        assert!(
            has_incoming_attack(&guest.games[0]),
            "AIが出した妨害岩も自分の盤面へ積まれるはず"
        );
    }

    #[test]
    fn only_the_host_runs_the_ai_and_the_guests_wait_for_the_proxy() {
        // #300: AIの判断を両側で回すとゲスト側で二重に適用されてしまうため、
        // `Autopilot`を持つのはホスト(room内インデックス0)だけにする。
        const HUMANS: usize = 2;
        const AIS: usize = 1;

        let (host, _host_peers) = battle_with_ai_slots(HUMANS, AIS, 0, 30005);
        assert_eq!(
            host.ai_pilots
                .iter()
                .filter(|pilot| pilot.is_some())
                .count(),
            AIS,
            "ホストはAIぶんのオートプレイを持つはず"
        );
        assert!(
            // `ai_pilots[0]`は`games[1]`=もう1人の人間に対応する。
            host.ai_pilots[0].is_none(),
            "人間の相手にオートプレイを持たせてはいけない"
        );

        let (guest, _guest_peers) = battle_with_ai_slots(HUMANS, AIS, 1, 30005);
        assert!(
            guest.ai_pilots.iter().all(Option::is_none),
            "ゲストはAIを動かさず、代理送信された入力を待つはず"
        );
    }

    #[test]
    fn the_host_proxies_the_ai_input_to_every_guest() {
        // #300: ホストはAIの判断を自分の手元へ適用しつつ、同じ操作を全ゲストへ
        // `proxy_for`付きで送る。これが無いとゲスト側のAIの盤面が止まって見える。
        const HUMANS: usize = 3;
        const AIS: usize = 1;
        const AI_ROOM_INDEX: usize = HUMANS;
        let (mut host, mut peers) = battle_with_ai_slots(HUMANS, AIS, 0, 30006);
        for peer in peers.iter_mut() {
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }

        // AIが1フレームで何も判断しないこともあるため、数フレームぶん回してから読む。
        // 自分の操作(`local_action`)は渡していないので、届く`Input`は代理送信だけ。
        for _ in 0..30 {
            host.pump_frame(None);
        }

        for (index, peer) in peers.iter_mut().enumerate() {
            let message =
                read_message_matching(peer, |message| matches!(message, GameMessage::Input { .. }));
            let GameMessage::Input { proxy_for, .. } = message else {
                unreachable!("Inputだけを選んで読んでいる");
            };
            assert_eq!(
                proxy_for,
                Some(AI_ROOM_INDEX),
                "peer{index}: AIの入力はroom内インデックス{AI_ROOM_INDEX}の代理として届くはず"
            );
        }
    }

    #[test]
    fn the_host_proxies_the_ai_result_exactly_once() {
        // #300: AIの枠が決着したら、その時点で1回だけ`Result`を代理送信する。毎フレーム
        // 送ると、ゲスト側で同じ結末を何度も受け取ることになる。
        const HUMANS: usize = 2;
        const AIS: usize = 1;
        const AI_ROOM_INDEX: usize = HUMANS;
        let (mut host, mut peers) = battle_with_ai_slots(HUMANS, AIS, 0, 30007);
        peers[0]
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // AIの枠(games[2])を決着させる。AIの手並みに左右されないよう、ゴール到達の
        // 結果を直接作る。
        host.games[2].status = GameStatus::Cleared;
        host.pump_frame(None);

        let message = read_message_matching(&mut peers[0], |message| {
            matches!(message, GameMessage::Result { .. })
        });
        assert!(
            matches!(
                message,
                GameMessage::Result {
                    reached_goal: true,
                    proxy_for: Some(AI_ROOM_INDEX),
                    ..
                }
            ),
            "AIの結末がroom内インデックス{AI_ROOM_INDEX}の代理として届くはず(届いたのは{message:?})"
        );

        // さらに回しても2通目は来ない。自分(ホスト)の決着はまだなので、自分ぶんの
        // `Result`も混ざらない。
        for _ in 0..30 {
            host.pump_frame(None);
        }
        peers[0]
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut extra_results = 0;
        while let Ok(message) = net::read_message(&mut peers[0]) {
            if matches!(message, GameMessage::Result { .. }) {
                extra_results += 1;
            }
        }
        assert_eq!(extra_results, 0, "同じ結末が2回以上送られている");
    }

    #[test]
    fn an_ai_slot_never_counts_as_a_disconnected_peer() {
        // #300: AIの枠は接続を持たないため切断し得ない。Heartbeatの送信先にも数えず、
        // タイムアウト判定でも触らない(触ると、いるはずのAIが勝手に脱落する)。
        const HUMANS: usize = 2;
        const AIS: usize = 2;
        let (mut host, _peers) = battle_with_ai_slots(HUMANS, AIS, 0, 30008);
        assert_eq!(
            host.peers
                .as_ref()
                .expect("通信ありの対戦状態のはず")
                .iter()
                .filter(|peer| peer.is_none())
                .count(),
            AIS,
            "前提: AIぶんは接続なしの枠のはず"
        );

        for _ in 0..30 {
            host.pump_frame(None);
        }

        // ホストから見るとAIの枠はgames[HUMANS..]に並ぶ(自分がroom内インデックス0)。
        for games_index in HUMANS..HUMANS + AIS {
            assert_eq!(
                host.games[games_index].status,
                GameStatus::Playing,
                "AIの枠(games[{games_index}])が切断扱いで倒されている"
            );
        }
    }

    #[test]
    fn a_room_with_two_humans_and_one_ai_looks_the_same_from_every_viewpoint() {
        // #300: 人間2人+AI1人のルームを実際の接続で組み、ホストが代理送信したAIの操作が
        // ゲストの手元で同じ盤面になることを見る。参加者の並び(表示名)も全員で揃うはず。
        const HUMANS: usize = 2;
        const AIS: usize = 1;
        const TOTAL: usize = HUMANS + AIS;
        const AI_ROOM_INDEX: usize = HUMANS;
        const SEED: u64 = 30009;
        let mut states = connected_mesh_with_ai(HUMANS, AIS, SEED);
        assert_eq!(states.len(), HUMANS, "`BattleState`を持つのは人間だけ");

        for (h, state) in states.iter().enumerate() {
            assert_eq!(
                state.games.len(),
                TOTAL,
                "参加者{h}: AIぶんも含めた全員の盤面を持つはず"
            );
            for p in 0..TOTAL {
                assert_eq!(
                    state.player_names[games_index_of(TOTAL, h, p)],
                    room_member_name(HUMANS, p),
                    "参加者{h}の視点で、room内インデックス{p}の表示名がずれている"
                );
            }
        }

        let ai_on_host = games_index_of(TOTAL, 0, AI_ROOM_INDEX);
        let ai_on_guest = games_index_of(TOTAL, 1, AI_ROOM_INDEX);
        let host_on_guest = games_index_of(TOTAL, 1, 0);
        let guest_on_host = games_index_of(TOTAL, 0, 1);

        // 人間2人は何も操作せず、AIだけがホストの手元で動く。全員を同じ回数・同じ
        // 経過時間で進めるので、「誰にも操作されていない盤面」は同じ手順で進めた
        // `untouched`と一致するはず。差が出る枠=操作が届いた枠になる。
        const FRAMES: usize = 60;
        let mut untouched = new_game_from_battle_config(SEED, &test_battle_config());
        for _ in 0..FRAMES {
            for state in states.iter_mut() {
                state.pump_frame(None);
            }
            untouched.update(TEST_FRAME_DELTA);
            // 代理送信が相手へ届く隙を作る。
            pump_interval();
        }

        assert!(
            !same_board_and_player(&states[0].games[ai_on_host], &untouched),
            "前提: ホストの手元でAIが操作されているはず"
        );
        assert!(
            !same_board_and_player(&states[1].games[ai_on_guest], &untouched),
            "ゲストの手元のAIの盤面が動いていない(代理送信が届いていない)"
        );
        assert!(
            same_board_and_player(&states[1].games[host_on_guest], &untouched),
            "AIの操作がゲストの手元にあるホストの盤面へ紛れ込んでいる"
        );
        assert!(
            same_board_and_player(&states[0].games[guest_on_host], &untouched),
            "AIの操作がホストの手元にあるゲストの盤面へ紛れ込んでいる"
        );
    }
}
