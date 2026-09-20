# 4人対戦 段階C: N人ハンドシェイク・ルーム参加フロー(#275)

#274で作ったフルメッシュのlockstep(`BattleState::from_peer_streams`)に、実際に「主催者が参加者を集めて、config/seedを配布し、全員がフルメッシュ接続を確立する」ところを実装する。ロビーUI(ルーム作成・参加者一覧表示・開始ボタンの操作性)は#276の範囲なので、この段階では「ネットワーク層の関数として、ブロッキングI/Oで完結する」形にする。#276は非ブロッキングな状態機械でこれをラップする。

## 1. 用語

- **ルーム参加接続**: 各参加者が主催者へ張る、開始トリガーが来るまでの一時的なTCP接続。`JoinRoom`の送信と、開始が決まった後の`RoomRoster`/`StartConfig`/`SeedAgree`/`StartCountdown`の受信にのみ使い、その後は用済みになる(閉じる)。
- **メッシュ接続**: 対戦本体で使う、参加者どうしの1対1のTCP接続(#274の`PeerLink`が持つもの)。各参加者は自分のメッシュ接続用`TcpListener`を1つ持つ。

## 2. プロトコル拡張(net.rs)

既存のHello→StartConfig→SeedAgree→StartCountdownの前に、ルーム参加のやり取りを挟む。

```rust
GameMessage::JoinRoom { name: String, mesh_port: u16 }
// 参加者→主催者(ルーム参加接続で送信)。名前と、自分のメッシュ接続用listenerのポート。

GameMessage::RoomRoster { members: Vec<RoomMember>, your_index: usize }
// 主催者→各参加者(ルーム参加接続で送信)。members[0]は常に主催者。
// your_indexは「このメッセージの送り先」自身のmembers内での位置(名前の重複が
// あっても各参加者が自分を一意に特定できるよう、ブロードキャストの内容を
// 送り先ごとに変えている)。
```

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomMember {
    pub name: String,
    pub mesh_addr: SocketAddr, // メッシュ接続の受け口(IP+ポート)
}
```

`StartConfig`/`SeedAgree`/`StartCountdown`は既存のまま、`RoomRoster`の直後に同じルーム参加接続で送る(#254の2人版の流用)。

## 3. 役割の決定規則

**room内インデックスが小さい方がTCPサーバ役**(#254の「承諾した側がサーバ」をN人へ一般化した、決定的な規則)。全員が同じ`members`を見ているため、誰がどちらの役になるかは通信なしに全員で一致する。

- 自分より小さいインデックスの参加者へは、自分から`mesh_addr`へconnectする(クライアント役)。
- 自分より大きいインデックスの参加者からの接続は、自分の`mesh_listener`で`accept`する(サーバ役)。到着順はインデックス順と一致しない場合があるため、接続直後に`Hello{name}`を1往復し、名前を`members`と照合して相手のインデックスを特定する。

これでC(n,2)本の接続それぞれについて、開始する側が両者で一致する。

## 4. 主催者側のフロー

1. ルーム参加用の`TcpListener`(既存のロビーの`listener`をそのまま使う)で、参加者からの接続を待つ。UIが「今何人集まっているか表示しながら、任意のタイミングで開始する」という操作性を持つため(#276)、この受け付け自体は#275の関数には含めない。呼び出し元が非ブロッキングに`accept`し、`JoinRoom`を受信してゲスト一覧を組み立てる(既存の2人版`accept_opponent`と同じパターン)。
2. 主催者が「開始」を決めたら、`start_room_as_host`を呼ぶ。この関数が、集まったゲストの接続一覧に対してRoomRoster以降を配布し、フルメッシュを確立して`Vec<TcpStream>`(自分以外、room内インデックス順)を返す。

```rust
/// 主催者側。既にJoinRoomを受け取った各ゲストとの接続(`guest_room_streams`。
/// JoinRoomを受信した順=room内インデックス1,2,3...と対応)に対して、
/// RoomRoster・対戦設定・シード・開始時刻を配布し、フルメッシュのメッシュ接続
/// (`Vec<TcpStream>`。自分以外、room内インデックス順)を確立する。
pub fn start_room_as_host(
    guest_room_streams: &mut [TcpStream],
    guest_names: &[String],
    guest_mesh_addrs: &[SocketAddr],
    my_name: &str,
    my_mesh_listener: &TcpListener,
    config: BattleConfig,
) -> io::Result<(Vec<TcpStream>, HandshakeResult)>
```

## 5. 参加者側のフロー

```rust
/// 参加者側。主催者へ接続してJoinRoomを送り、RoomRoster以降を受け取ってから
/// フルメッシュを確立する。
pub fn join_room_as_guest(
    host_addr: SocketAddr,
    my_name: &str,
    my_mesh_listener: &TcpListener,
) -> io::Result<(Vec<TcpStream>, Vec<String>, HandshakeResult)>
// 戻り値: (自分以外とのメッシュ接続。room内インデックス順, 自分以外の名前を
// 同じ順で並べたもの, ハンドシェイク結果)
```

## 6. フルメッシュ確立の共通ロジック

主催者・参加者の双方が「自分のroom内インデックス」と「members一覧」を受け取った後は、同じ関数でメッシュ接続を確立できる。

```rust
/// `members`(room内インデックス順、自分を含む)と自分のインデックス`my_index`から、
/// 自分以外の全員とのメッシュ接続を確立する(#275)。戻り値は`members`から自分を
/// 除いた順(`BattleState::from_peer_streams`が要求する、games[1..]と対応する順)。
fn establish_full_mesh(
    members: &[RoomMember],
    my_index: usize,
    my_name: &str,
    mesh_listener: &TcpListener,
) -> io::Result<Vec<TcpStream>>
```

- `0..my_index`の各`members[i]`へ`TcpStream::connect_timeout`し、`Hello`を送ってから相手の`Hello`を受け取る(2人版`run_client_handshake`と同じ順序)。
- 残り(`members.len() - my_index - 1`本)を`mesh_listener.accept()`で受け入れ、`Hello`を受け取ってから自分の`Hello`を返し(2人版`run_host_handshake`と同じ順序)、名前で`members`と照合して挿入位置を決める。
- 全て揃ったら、`members`の順から自分を除いた並びで返す。

## 7. 次段階との境界

- #276: ロビーUIをこのAPI群でラップする。「ルーム作成→参加者が増えるたびに一覧を更新表示→ホストがEnterで開始」という非ブロッキングな状態機械を`lobby.rs`に実装し、`BattleState::from_peer_streams`を呼び出す経路を完成させる。UDP探索(`discovery.rs`)をN人の「誰が主催者か」の発見にどう使うか(招待の宛先を複数人に広げる等)も#276で決める。
