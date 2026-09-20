# 4人対戦 段階D: ロビーUIのN人対応(#276)

#275の`room.rs`(ルーム参加フロー・フルメッシュ確立)を、実際にロビー画面(`lobby.rs`)から呼び出せるようにする。#273/#274/#275はここまでテストコードからしか使われていなかった。

## 1. 役割の反転

既存の2人版は「招待を承諾した側がTCPサーバ役」だったが、N人版では**「招待した側(自分から誘う側)が常にルームの主催者(サーバ役)」**に反転する。「自分が複数人を誘って集める」という#276の主目的に、この方が直感的に合う。

- 招待した側 → ACCEPTを受けたら、そのゲストのTCP接続を受け入れる(主催者)
- 招待を承諾した側 → 主催者へ接続してルームに加わる(ゲスト)

## 2. `LobbyPhase`の拡張

```rust
pub enum LobbyPhase {
    /// `guests`が空でなければ、既にルームを開いていて追加の招待を送れる状態。
    Discovering { guests: Vec<HostedGuest> },
    AwaitingInviteResponse { target: DiscoveredPeer, sent_at: Instant, guests: Vec<HostedGuest> },
    IncomingInvite { from: DiscoveredPeer },
    /// ACCEPTを受けたゲストのTCP接続を待っている(主催者側)。
    AcceptingGuestConnection { guest_name: String, started: Instant, guests: Vec<HostedGuest> },
    /// 招待を承諾し、主催者への接続を試みている(ゲスト側)。
    ConnectingToHost { addr: SocketAddr, host_name: String },
    /// 主催者へ接続・JoinRoom送信済みで、開始(RoomRoster以降)を別スレッドで待っている。
    WaitingForRoomStart { result_rx: mpsc::Receiver<io::Result<RoomStartResult>> },
    Notice { message: String, shown_at: Instant },
}

/// 主催者が既に迎え入れたゲスト1人ぶん。
struct HostedGuest { room_stream: TcpStream, name: String, mesh_addr: SocketAddr }
```

## 3. キー割り当て

`Discovering`で`guests`が1人以上いる状態のとき、新規`InputAction::StartRoom`(Tabキーに割り当て)で開始する。既存のConfirm(Enter)は「選択中の候補へ招待」のまま変えない(誤操作で開始してしまうのを避ける)。

## 4. 主催者側フロー

1. `Discovering`で候補にConfirm→`AwaitingInviteResponse { guests }`(既存のguestsを持ち越す)
2. ACCEPT受信→`AcceptingGuestConnection { guest_name, started, guests }`
3. `listener.accept()`(既存、非ブロッキング)→接続できたら`JoinRoom`を1回読む(短時間のブロッキング、既存の`host_handshake`と同じ許容パターン)→`guests`に追加して`Discovering { guests }`に戻る
4. `StartRoom`入力(guests.len() >= 1)→`room::start_room_as_host`を呼び(ブロッキング、対戦成立までの一度きりの処理として既存パターンと同じ扱い)、`BattleState::from_peer_streams`を組み立てて`LobbyOutcome::Battle`を返す

主催者は`listener`(ルーム参加接続用)と`mesh_listener`(メッシュ接続用)の2つのTCP listenerを持つ(`bind_battle_listener()`をもう一度呼べば、既存の1つ目とは別の空きポートが自然に確保される)。

## 5. ゲスト側フロー

1. `IncomingInvite`でConfirm(承諾)→ACCEPT送信→`ConnectingToHost { addr, host_name }`(自分がクライアント役)
2. `TcpStream::connect_timeout`(既存の`connect_to_opponent`と同じ)→接続できたら`JoinRoom`を送信(`room::connect_and_join_room`、新設)→`WaitingForRoomStart { result_rx }`
3. `result_rx`は別スレッドで`room::await_room_start`(新設、既存`join_room_as_guest`の後半を分離したもの)を実行した結果を受け取る。主催者がいつ開始するか分からず無期限に待つ処理のため、メインループをブロックしないよう別スレッド化する
4. 結果が届いたら`BattleState::from_peer_streams`を組み立てて`LobbyOutcome::Battle`

`room.rs`の`join_room_as_guest`を2分割する(既存の呼び出し元・テストへの影響が無いよう、`join_room_as_guest`自体は分割した2関数を順に呼ぶだけの薄い関数として残す):

```rust
pub fn connect_and_join_room(host_addr: SocketAddr, my_name: &str, my_mesh_listener: &TcpListener) -> io::Result<TcpStream>;
pub fn await_room_start(room_stream: TcpStream, my_name: &str, my_mesh_listener: &TcpListener) -> io::Result<(Vec<TcpStream>, Vec<String>, net::HandshakeResult)>;
```

## 6. 既存テストへの影響

- `LobbyPhase::Discovering`が`{ guests }`を持つため、既存テストのパターンマッチを`Discovering { .. }`に直す
- 2人版の役割検証テスト(`accepting_an_invite_connects_both_sides_and_starts_a_battle`)は、役割反転後の期待値(招待した側=主催者/サーバ、承諾した側=ゲスト/クライアント)に書き直す
- 新規: 3人・4人でロビーを回し、対戦成立まで到達することを確認する結合テストを追加する
