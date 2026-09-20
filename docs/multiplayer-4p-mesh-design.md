# 4人対戦 段階B: フルメッシュ通信(#274)

#273で作った`Vec<Game>`ベースの`BattleState`(N人分の順位判定、通信なし)に、実際のフルメッシュ通信(自分以外の全参加者と直接TCP接続)を組み込む。ハンドシェイク・ルーム参加フロー(誰が接続を作るか)は#275の範囲なので、この段階では「確立済みのTCP接続がN-1本渡されたら、それを使って対戦を進められる」ところまでを実装する。

## 1. 設計方針: ペアごとに既存の2人版プロトコルを流用する

N人対応にあたり、`GameMessage`(net.rs)の構造は変更しない。フルメッシュでは自分と各peerが1本ずつ独立したTCP接続を持つため、**Input/Heartbeat/StateHash/Result/Byeの交換は、既存の2人版ロジックをそのまま「各peerごとに」適用するだけで足りる**。

具体的には、peer `i`(自分以外の参加者、`games[i+1]`に対応)との接続では:
- `Input`はそのpeer専用のtick番号で管理する(peerごとに`next_tick`を持つ。実際には全peerが同じtick数になるはずだが、届く順序はpeerごとに独立)
- `StateHash { tick, local_hash, remote_hash }`の`local_hash`は自分の`games[0]`、`remote_hash`は自分の`games[i+1]`(そのpeerに対応するインスタンス)のハッシュ。相手も同じ規約で送ってくるため、2人版と全く同じ照合ロジック(`local`⟷相手の`remote`、`remote`⟷相手の`local`)がそのまま使える
- `Result { reached_goal, tick, time_ms }`も「自分がゴールしたか」を全peerへ個別に送り、各peerからの申告を自分の`games[i+1]`と照合する(2人版と同じ)

こうすることで、既存の`take_remote_input_for`・`reconcile_state_hash`・`maybe_send_result`・`reconcile_remote_result`のロジックを、対象を「1つのNetworkLink」から「複数のPeerLinkそれぞれ」に広げるだけで再利用できる。

## 2. データ構造

```rust
pub struct BattleState {
    games: Vec<Game>,
    player_names: Vec<String>,
    net_tick_accum: Duration,
    ranks: Vec<Option<u8>>,
    next_win_rank: u8,
    next_lose_rank: u8,
    outcome: Option<BattleOutcome>,
    /// 自分以外の各参加者との通信路。`peers[i]`は`games[i+1]`に対応する
    /// (自分がgames[0]なので、games[1..]とpeers[0..]が1対1)。`None`なら通信なし
    /// (#273までのローカル専用テストの前提。既存テストは変更しない)。
    peers: Option<Vec<PeerLink>>,
}

/// 対戦相手1人との通信路(#274。#254の`NetworkLink`を複数保持できるよう改名し、
/// フィールドはそのまま持ち越す)。
struct PeerLink {
    writer: TcpStream,
    event_rx: mpsc::Receiver<NetworkEvent>,
    _receiver_thread: thread::JoinHandle<()>,
    next_tick: u32,
    committed_local_action: Option<NetAction>,
    pending_remote_inputs: VecDeque<(u32, NetAction)>,
    awaiting_since: Option<Instant>,
    last_remote_activity: Instant,
    last_heartbeat_sent: Instant,
    start_at_unix_ms: u64,
    result_sent: bool,
    remote_result: Option<(bool, u32)>,
    disconnected: bool,
    own_state_hashes: HashMap<u32, (u64, u64)>,
    pending_remote_state_hashes: HashMap<u32, (u64, u64)>,
}
```

## 3. 切断・タイムアウトしたpeerの扱い(重要な設計判断)

フルメッシュを選んだ狙い(単一障害点を避ける)に合わせ、**1人が切断・タイムアウトしても残りの参加者で対戦を続行する**(全員終了にはしない)。

切断・タイムアウトを検知したpeer `i`について:
- `games[i+1].status`を`GameOver`へ強制的に上書きする(実際の脱落ではないが、順位確定のためだけにこの状態を使う。ゲームロジック上の副作用は生じない——盤面はもう進めないため)
- 直後に`update_ranks()`を呼び、そのpeerの順位を確定させる(通常の脱落と同じ経路で`next_lose_rank`から割り振られる)
- そのpeerの`disconnected = true`にし、以後そのpeerからの入力待ちをしない(`take_remote_input_for`の対象から外す)

これにより、「切断者は最下位側から順位が埋まっていく」という自然な扱いになる。残りの全peerが揃えばtickは進み続け、生存者だけの対戦として決着まで進む。

## 4. tickの進行ロジック(N人版`advance_networked`)

```
advance_networked(delta, local_action):
    for peer in peers: drain_events(peer)  // Input/Heartbeat/StateHash/Result/Byeの受信処理
    if outcome.is_some():
        for peer: reconcile_remote_result(peer)
        return

    // 新たに切断・タイムアウトを検知したpeerがいれば、games[i+1]をGameOverにして
    // update_ranksを呼ぶ(まだ順位未確定のpeerのみ処理する)。
    for (i, peer) in peers.iter_mut().enumerate():
        if peer.disconnected && ranks[i+1].is_none():
            games[i+1].status = GameOver
            update_ranks()
        if let Some(since) = peer.awaiting_since:
            if since.elapsed() >= LOCKSTEP_WAIT_TIMEOUT_MS:
                peer.disconnected = true
                games[i+1].status = GameOver
                update_ranks()

    if outcome.is_some(): return  // 自分の順位が確定した(生存者側で決着した等)

    // まだ未確定(切断していない)peer全員がawaiting_sinceでないか確認。
    if any (未切断の) peer.awaiting_since.is_some():
        return  // 誰かの入力をまだ待っている。時計を進めない。

    for peer: Heartbeat送信判定(既存の間隔ロジックのまま、peerごとに)

    net_tick_accum += delta.min(DELTA_CLAMP_MS)
    while net_tick_accum >= NET_TICK_MS:
        my_action = 確定・全ての未切断peerへ送信済みか確認(committed_local_actionはpeerごとに持つ
                     必要はなく、1つの値を全peerへ送るだけでよい。#274ではBattleState側に
                     1つの`committed_local_action: Option<NetAction>`を持たせ、全peer分の送信が
                     完了したら消す)

        // 全ての未切断peerについて、該当tickの入力が揃っているか確認する。
        let mut all_actions = vec![my_action.into()]; // games[0]分
        let mut all_ready = true
        for (i, peer) in peers.iter_mut().enumerate():
            if peer.disconnected:
                all_actions.push(None) // 切断済み。run_tick_nはNoneとして扱う(GameOver状態の
                                        // Gameにupdateを掛けても実害は無いが、明示的にNoneでよい)
                continue
            match take_remote_input_for(peer, peer.next_tick):
                Some(action) => all_actions.push(action.into()),
                None => { peer.awaiting_since = Some(now); all_ready = false }

        if not all_ready: break  // このtickぶんはnet_tick_accumから引かず持ち越す

        net_tick_accum -= NET_TICK_MS
        committed_local_action = None
        for peer (未切断): peer.next_tick += 1
        let completed_tick = (どのpeerのnext_tickでもよい。全peer共通のtickカウンタを
                               BattleState側に1つ持たせる方が単純)

        run_tick_n(&mut games, &all_actions)
        update_ranks()

        // StateHash交換(設計書1節の方針。20tickごと)。
        if completed_tick % STATE_HASH_INTERVAL_TICKS == 0:
            let local_hash = games[0].state_hash()
            for (i, peer) in peers.iter_mut().enumerate() (未切断のみ):
                let remote_hash = games[i+1].state_hash()
                peer.own_state_hashes.insert(completed_tick, (local_hash, remote_hash))
                write StateHash{tick: completed_tick, local_hash, remote_hash} to peer.writer
                reconcile_state_hash(peer, &mut outcome, completed_tick)  // 既存ロジックそのまま

        if outcome.is_some():
            for peer: maybe_send_result(peer); reconcile_remote_result(peer)
            break
```

`committed_local_action`と`next_tick`はpeerごとに独立させず**BattleStateに1つずつ持たせる**方が単純になる(全peerが同じtickを共有するのが前提のため)。ただし、`pending_remote_inputs`・`awaiting_since`・StateHash関連のHashMapはpeerごとに独立して持つ(相手ごとに届くタイミングが違うため)。

設計者への注意: 上記は擬似コードであり、Rustの借用チェッカー上、`peers`と`games`を同時に可変参照する箇所(特にStateHash計算)で工夫が必要になる(#254の既存コードが`self.network.as_mut().expect(...)`を都度呼び直していたのと同じパターンで対応できるはず)。

## 5. コンストラクタ

既存の`from_handshake`(#254、2人専用)は**削除せず**、内部で新しいN人対応の組み立て処理へ委譲するようリファクタリングする(呼び出し元の`lobby.rs`を変更しないため)。

```rust
impl BattleState {
    /// N人分の確立済みTCP接続から通信ありの対戦状態を組み立てる(#274)。
    /// `games`/`player_names`は既にseed/configから生成済み(index 0が自分)で、
    /// `streams`は自分以外の各参加者との接続(`games`のindex 1..と対応する順)。
    /// `start_at_unix_ms`はハンドシェイクで合意した開始時刻(Result送信時の経過算出に使う)。
    pub fn from_peer_streams(
        games: Vec<Game>,
        player_names: Vec<String>,
        streams: Vec<TcpStream>,
        start_at_unix_ms: u64,
    ) -> io::Result<Self> {
        debug_assert_eq!(games.len(), player_names.len());
        debug_assert_eq!(streams.len(), games.len() - 1);
        let player_count = games.len();
        let peers = streams.into_iter().map(|stream| PeerLink::new(stream, start_at_unix_ms))
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Self {
            games, player_names,
            net_tick_accum: Duration::ZERO,
            ranks: vec![None; player_count],
            next_win_rank: 1,
            next_lose_rank: player_count as u8,
            outcome: None,
            peers: Some(peers),
        })
    }

    /// #253のハンドシェイク結果からの2人専用の組み立て(#254)。#275でN人対応の
    /// ハンドシェイクが実装されるまでの間、既存のロビーフロー(`lobby.rs`)はこの
    /// シグネチャのまま呼び続けられるよう、内部で`from_peer_streams`へ委譲する。
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
        Self::from_peer_streams(games, player_names, vec![stream], handshake.start_at_unix_ms)
    }
}
```

`PeerLink::new(stream, start_at_unix_ms)`は既存`from_handshake`内で行っていた`try_clone`+`spawn_receiver_thread`+フィールド初期化をまとめたヘルパーとして新設する。

## 6. テスト方針

- **3人・4人のフルメッシュ結合テスト**: ループバックTCPで各participantが他の全員とペアの接続を作り(3人ならC(3,2)=3本、4人ならC(4,2)=6本)、`from_peer_streams`で各参加者の`BattleState`を組み立て、異なる入力列を送り合っても全員の`games`が一致し続けることを確認する(#251/#254の`assert_lockstep_matches`・`two_hosts_connected_over_tcp_advance_in_lockstep`の一般化)
- **1人の切断で残り3人が続行するテスト**: 4人のうち1人が`Bye`を送って抜けた後、残り3人だけで対戦が進み、抜けた人が最下位で確定することを確認する
- **1人のタイムアウトで残りが続行するテスト**: 同様に、1人が入力を送らなくなった場合(`LOCKSTEP_WAIT_TIMEOUT_MS`超過)
- **StateHash不一致でDesyncになるテスト**: ペアの1つで食い違う値を送りつけ、そのpeerとの照合が`Desync`になることを確認する(N人版でも、どのpeerとの不一致でも`Desync`に落ちることを確認する)
- **既存の2人専用テスト(#254で書かれた`connected_pair`ベースのテスト群)は`from_handshake`経由のまま変更不要**であることを確認する(委譲リファクタリングが後方互換であることの裏付け)

## 7. 次段階との境界

- #275: N人分のハンドシェイク(主催者がconfig/seedを配布)・ルーム参加(新規参加者が既存の参加者全員とフルメッシュ接続を確立)を実装し、`from_peer_streams`を実際に呼び出す経路を作る
- #276: ロビーUIをN人対応に拡張し、タイトルから実際にN人対戦を始められるようにする
