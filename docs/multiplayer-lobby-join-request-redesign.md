# ロビー: 招待モデル→参加リクエストモデルへの再設計(#293)

`docs/multiplayer-4p-lobby-design.md`(#276)で導入した「招待した側が常に主催者」モデルは、#291で限界が判明した。ホストが1件の招待を検討中(`IncomingInvite`)の間に別の招待が届くと処理できず、送った側は`INVITE_TIMEOUT_MS`(10秒)まで無応答で待たされる。#291では応急対応として即座にDeclineする修正を入れたが、根本的にはモデル自体を見直す。

## 1. 新モデルの概要

- **ホスト側はルームを作成する**: 現行通り、ロビーに入った時点で暗黙的に募集中(誰でも参加リクエストを送れる)状態になる。明示的な「ルームを作る」操作は追加しない。
- **対戦申し込み側がそのルームに参加する**: 探索リストから相手を選んでEnterを押す操作は、**常に「参加リクエストを送る」**意味になる。送った側は常にゲストになる(#276の「役割の反転」からさらに反転し、初代の招待モデルの力学に戻る形)。
- **参加許可/拒否はホストが個別に行う**: 複数の参加リクエストが同時に来た場合、1件ずつ順番に確認する(キュー)。#291のように無視される参加者を出さない。
- **ゲーム開始はホストまたは参加者のどちらでもできる**: 参加者が開始操作をしたら、ホストの追認なしに即座に開始する。

### 削除する機能

#276で追加した「ホストが探索リストから相手を選んで自分から誘う」機能は削除する。Enterキーの意味を「参加リクエスト送信」に統一するため、探索リストにいる間は常に自分がリクエスト送信者(将来のゲスト)になる。ホストが積極的に人を集める手段は無くなり、募集して待つだけになる。

## 2. 役割の反転(#276からさらに反転)

- 参加リクエストを送った側 → 応答を待ち、許可(ACCEPT)を受けたらホストへ接続する(ゲスト)
- 参加リクエストを受けた側 → 許可すると、そのゲストのTCP接続を受け入れる(ホスト。既に迎え入れているゲストがいてもホストのまま)

これは`docs/multiplayer-4p-lobby-design.md`の「1. 役割の反転」が導入した力学(招待した側=ホスト)を、UDPパケットの意味ごと反転させる。パケットタイプ自体(`Invite`/`Accept`/`Decline`)は名前も含めて変えない(名前が指す操作の意味は「対戦を申し込む」で違和感がないため)。変わるのは`packet_effect`/`apply_action`内の遷移先だけ。

## 3. `LobbyPhase`の変更

```rust
pub enum LobbyPhase {
    Discovering { guests: Vec<HostedGuest> },
    /// 自分が参加リクエストを送り、応答を待っている(名前は既存を維持)。
    AwaitingInviteResponse { target: DiscoveredPeer, sent_at: Instant, guests: Vec<HostedGuest> },
    /// 参加リクエストを受け取り、許可するか判断している。
    /// `pending`は他に届いている未処理の参加リクエスト(#291。1件ずつ順番に処理する)。
    IncomingInvite {
        from: DiscoveredPeer,
        pending: Vec<DiscoveredPeer>,
        guests: Vec<HostedGuest>,
    },
    /// ACCEPTを送ったゲストのTCP接続を待っている(ホスト側。既存のまま)。
    /// `pending`をここにも持たせ、接続待ち中に届いた新規リクエストを取り逃さない。
    AcceptingGuestConnection {
        guest_name: String,
        started: Instant,
        pending: Vec<DiscoveredPeer>,
        guests: Vec<HostedGuest>,
    },
    /// 参加リクエストの許可を受け、ホストへ接続を試みている(ゲスト側。既存のまま)。
    ConnectingToHost { addr: SocketAddr, host_name: String },
    /// ホストへ接続・JoinRoom送信済みで、開始を別スレッドで待っている(既存のまま)。
    /// このフェーズでStartRoom操作を受け付け、ホストへ開始要求を送る(新規)。
    WaitingForRoomStart { result_rx: mpsc::Receiver<io::Result<RoomStartResult>> },
    /// `pending`はここにも持たせる(通知表示中に届いたリクエストを取り逃さない。#284と同じ理由)。
    Notice { message: String, shown_at: Instant, guests: Vec<HostedGuest>, pending: Vec<DiscoveredPeer> },
}
```

`pending`はほぼ全フェーズに引き継がせる必要がある(#284で`guests`を持ち越したのと同じ理由: 通知やホストの接続待ちを挟んでも参加リクエストを取りこぼさない)。Discoveringへ戻る際、`pending`の先頭を取り出して次の`IncomingInvite`として提示する(空なら通常のDiscoveringへ)。

## 4. `packet_effect`/`apply_action`の変更点

- `(Discovering, Invite)`: 現行は`guests.is_empty()`で「初めての招待ならIncomingInvite、既にゲストがいればDeclineWhileHosting」だった。新設計では**guestsの有無にかかわらずIncomingInviteとして受け付ける**(N人対戦なので、ゲストがいてもさらに参加リクエストを受けられる)。`ROOM_MAX_PLAYERS`上限のみ、上限到達時はDeclineWhileHostingのまま維持する。
- `(IncomingInvite { pending, .. }, Invite)`(#291で追加したケース): 「即座にDecline」から**「pendingへ積む」**に変更する。これが#291の症状(後着が無応答で待たされる)を根本的に解消する。
- `IncomingInvite`のConfirm処理: 現行「ACCEPT送信→ConnectingToHost(自分がゲストになる)」から、**「ACCEPT送信→AcceptingGuestConnection(自分はホストのまま、相手の接続を待つ)」**に変える。
- `AwaitingInviteResponse`がAccept受信した時の処理: 現行「AcceptingGuestConnection(自分がホストになる)」から、**「ConnectingToHost(自分がゲストになる)」**に変える。

## 5. ゲストからの開始要求(新規)

`WaitingForRoomStart`中にゲストが`InputAction::StartRoom`を押したら、既存のTCP接続(`room_stream`)を使わず、UDP探索ソケット経由でホストへ新規パケット`PacketType::RequestStart`を送る(理由: `room_stream`はホスト→ゲスト方向の一方通行で運用されており、ゲスト→ホスト方向の継続的な読み取りをホスト側に追加するより、既に非ブロッキングでpollしている探索ソケットの枠に乗せる方が変更が小さい)。

ホスト側は`Discovering`/`AcceptingGuestConnection`中に`RequestStart`を受信したら、`guests`が1人以上いることを確認し、即座に`room::start_room_as_host`を呼ぶ(ホストの追認なし=確認画面を出さない)。

## 6. 既存テストへの影響

- 役割反転検証テスト(`accepting_an_invite_connects_both_sides_and_starts_a_battle`ほか)を新しい期待値(参加リクエストを送った側=ゲスト/クライアント、受けた側=ホスト/サーバ)に書き直す
- `a_second_invite_that_arrives_while_deciding_on_the_first_is_declined_immediately`(#291)を、pendingキューに積まれて後で処理されることを検証する内容に書き直す(テスト名も変更)
- `an_invite_that_arrives_while_hosting_a_room_is_declined_automatically`(#276): guestsがいてもIncomingInviteとして受け付けるようになるため、この動作自体を削除する(上限到達時のDeclineWhileHostingテストは残す)
- 新規: ゲストが`StartRoom`を押すと、ホストの追認なしに対戦が始まることを確認する結合テスト
- 新規: 複数の参加リクエストが1件ずつ順番に処理されることを確認する結合テスト(3人以上)
