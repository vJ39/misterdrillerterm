//! 対戦の通信層(#253。spec.md 12.1・12.2)。
//!
//! TCP接続が確立済みの2ホストが、メッセージのフレーミングを介してハンドシェイク
//! (Hello交換 → StartConfig → SeedAgree)を行うところと、対戦中の
//! 受信専用スレッド(#254)、およびUDP探索のパケット形式(#256)を担う。
//! 受信したメッセージを自分のシミュレーションへどう反映するか(#254)はゲームロジック側の
//! 責務のため`battle.rs`に置く。ハンドシェイクの結果から`Game`を組み立てるのも同じ理由で
//! `battle::new_game_from_battle_config`に置く。
//!
//! #256でUDP探索(spec.md 12.1)のパケット定義も加えた。探索の状態管理そのものは
//! `discovery.rs`、招待のやり取りとタイトルからの入口は`lobby.rs`が持つ。

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;
use std::thread;

use rand::RngExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::game::InputAction;
use crate::settings::Settings;

/// TCP接続後のメッセージの版(#317)。`Hello`・`JoinRoom`(#325)で相手と交換する。
///
/// `GameMessage`・`BattleConfig`の構造を変える変更をするときは、必ずこの値を1つ上げる。
/// 違う版どうしが繋がると、同じバイト列を別の構造として読むため、デシリアライズが
/// 失敗するか、運が悪いと成功した上で中身が食い違う。どちらも原因の分かりにくい
/// 不具合になるので、対戦が始まる前にここで弾く。
///
/// `Hello`へ項目を足すときは`protocol_version`より後ろへ足す。デコードは余った
/// バイトを読み飛ばすため、古い版でも名前と版までは読めて不一致を報告できる。
///
/// UDP探索のパケットには別の版(`DISCOVERY_PROTOCOL_VERSION`)がある。
pub(crate) const PROTOCOL_VERSION: u32 = 1;

/// TCP接続後にやり取りするメッセージ(spec.md 12.2)。
///
/// `Hello`〜`SeedAgree`が開始前のハンドシェイク用、`Input`以降が対戦中用。
/// 対戦中のメッセージはtick番号を持たない。各参加者が自分の実時間でシミュレーションを
/// 進める非同期方式のため、届いた順にそのまま反映すればよい(spec.md 12.3)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameMessage {
    Hello {
        name: String,
        /// 送信側の`PROTOCOL_VERSION`(#317)。受信側は自分の値と比べ、違えば
        /// 対戦を始めずに切る。
        protocol_version: u32,
    },
    /// 参加者→主催者(ルーム参加接続で送信)。N人対戦のルームへ参加を申し込む(#275)。
    /// `mesh_port`は自分のメッシュ接続用listenerのポートで、主催者はこれに接続元のIPを
    /// 添えて`RoomRoster`の`mesh_addr`を組み立てる。
    JoinRoom {
        name: String,
        mesh_port: u16,
        /// 送信側の`PROTOCOL_VERSION`(#325)。`Hello`と同じ判定をルーム参加の時点でも
        /// 行うことで、`StartConfig`のデコード失敗という分かりにくいエラーになる前に
        /// 版の不一致を検知できる。
        protocol_version: u32,
    },
    /// 主催者→各参加者(ルーム参加接続で送信)。`members[0]`は常に主催者(#275)。
    ///
    /// `your_index`は「このメッセージの送り先」自身の`members`内での位置。名前が
    /// 重複していても各参加者が自分を一意に特定できるよう、ブロードキャストの内容を
    /// 送り先ごとに変えている。
    RoomRoster {
        members: Vec<RoomMember>,
        your_index: usize,
    },
    /// ホスト(TCPサーバ役)のシミュレーション影響設定一式。クライアントは
    /// この値を対戦セッション中のみ強制適用する(自分のsettings.jsonへは保存しない)。
    ///
    /// 設定一式は項目数が多く(#312で20項目増えた)、このenum全体の大きさを決めてしまう。
    /// 対戦中は`Input`等の小さなメッセージを毎フレーム扱うため、大きいのはこの1つだけに
    /// 留めたい。ハンドシェイクで1回しか送らないので、間接参照にしても割に合う。
    /// 直列化の結果は中身をそのまま書くだけで変わらないため、通信の互換性には影響しない。
    StartConfig(Box<BattleConfig>),
    SeedAgree {
        /// 人間の参加者が共有するシード。全員が同じ盤面を掘る公平な条件にするため1つだけ配る。
        seed: u64,
        /// AI(#300)の枠ぶんのシード(roster内のAIの並び順)。#319: オートプレイは
        /// 決定論的に動くため、AIが複数いても同じ盤面だと結果がほぼ同じになる。
        /// ゲスト側もホストが動かしているAIと同じ盤面のコピーを持つ必要があるため、
        /// ホストがまとめて生成して配る。
        ai_seeds: Vec<u64>,
    },
    /// 自分の操作1つ。受け取った側は自分が持つ送信元のインスタンスへ即座に適用する。
    /// TCPが順序を保証するため、送った順=適用される順になる。
    Input {
        action: NetAction,
        /// 代理対象のroom内インデックス。`None`なら送信者自身の入力。
        /// `Some`はホストがAI(#300)の入力を代理送信する場合のみ使う。
        proxy_for: Option<usize>,
    },
    /// 生存確認のみ(spec.md 12.4)。一定時間これも`Input`も届かなければ切断とみなす。
    Heartbeat,
    /// 妨害(#247/#297/#304)。自分が消したブロック数を、岩ぶん・ボムぶんへ振り分けた形で
    /// 相手へ送る。受け取った側は自分がまだPlayingのときだけ適用する(spec.md 12.8)。
    /// 種類ごとに別勘定で相殺するため2つの量を持つが、メッセージは1通にまとめる。
    Attack {
        rock_amount: u32,
        bomb_amount: u32,
        /// `Input`と同じ意味。`Some`はホストがAI(#300)の妨害を代理送信する場合のみ。
        proxy_for: Option<usize>,
    },
    Result {
        reached_goal: bool,
        /// `Input`と同じ意味。`Some`はホストがAI(#300)の結果を代理送信する場合のみ。
        proxy_for: Option<usize>,
    },
    Bye,
}

/// ルームの参加者1人ぶんの情報(#275)。`RoomRoster`で全参加者へ同じ並びを配り、
/// この並びの位置(room内インデックス)が対戦中の参加者番号とメッシュ接続の役割
/// (どちらがTCPサーバ役か)を決める。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomMember {
    pub name: String,
    /// メッシュ接続の受け口(IP+ポート)。`None`はAI(#300。ホストがローカルで操作し、
    /// 入力・妨害岩・結果を代理送信する追加参加者。実際のTCP接続を持たない)。
    pub mesh_addr: Option<SocketAddr>,
}

/// 1章の`InputAction`のうちネットワーク同期に必要な要素のみを送る(spec.md 12.2)。
/// TogglePause/Quit/ToggleMusic/ToggleSe/Debug*系はローカルのみで完結させ
/// (12.5の通り対戦中はほぼ無効化)、送信の対象にしない。
///
/// `game::InputAction`と役割が重なるが、あちらはキー入力から得られる全アクションを
/// 持つゲーム内部の型で、こちらはプロトコル上の型として独立させる(名前の衝突も避ける)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetAction {
    None,
    MoveLeft,
    MoveRight,
    FaceUp,
    FaceDown,
    Drill,
}

impl From<NetAction> for Option<InputAction> {
    /// 受信した相手の入力を、`Game::apply_input`へ渡す形へ変換する。
    /// `NetAction::None`は「操作なし」を表すため`None`になる。
    fn from(action: NetAction) -> Self {
        match action {
            NetAction::None => None,
            NetAction::MoveLeft => Some(InputAction::MoveLeft),
            NetAction::MoveRight => Some(InputAction::MoveRight),
            NetAction::FaceUp => Some(InputAction::FaceUp),
            NetAction::FaceDown => Some(InputAction::FaceDown),
            NetAction::Drill => Some(InputAction::Drill),
        }
    }
}

impl From<InputAction> for Option<NetAction> {
    /// 自分の入力を送信用へ変換する。対戦tickに含めない操作(一時停止・デバッグ系等)は
    /// `None`になる。
    fn from(action: InputAction) -> Self {
        match action {
            InputAction::MoveLeft => Some(NetAction::MoveLeft),
            InputAction::MoveRight => Some(NetAction::MoveRight),
            InputAction::FaceUp => Some(NetAction::FaceUp),
            InputAction::FaceDown => Some(NetAction::FaceDown),
            InputAction::Drill => Some(NetAction::Drill),
            _ => None,
        }
    }
}

/// シミュレーション結果に影響する設定の完全な集合(spec.md 12.2)。設定画面の項目のうち
/// music/SE/debug_log等のローカル専用項目は含めない。1つでも食い違うと決定性が崩れる
/// ため、「両者で揃える」のではなくホストの値を一方的に採用する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BattleConfig {
    /// 対戦のゴール深度(m)。ホストがモードセレクト(#112)で選んだコースに従う。
    pub depth_goal_m: u32,
    pub field_width: u32,
    pub rock_spawn_rate_percent: u32,
    pub air_spawn_rate_percent: u32,
    pub star_spawn_rate_percent: u32,
    pub diamond_spawn_rate_percent: u32,
    pub item_clear_above_rate_percent: u32,
    pub item_unify_colors_rate_percent: u32,
    pub item_starify_screen_rate_percent: u32,
    pub color_count: u8,
    pub color_cluster_rate_percent: u32,
    pub bomb_spawn_rate_percent: u32,
    pub bomb_fuse_ms: u32,
    pub attack_blocks_per_rock: u32,
    pub attack_rocks_per_wave_max: u32,
    pub attack_blocks_per_bomb: u32,
    pub attack_bombs_per_wave_max: u32,
    pub attack_bomb_ratio_percent: u32,
    pub block_fall_tick_ms: u64,
    pub player_fall_tick_ms: u64,
    pub shake_duration_ms: u64,
    pub move_cooldown_ms: u64,
    pub dodge_recovery_ms: u64,
    pub chain_vanish_interval_ms: u64,
    // 以下はAI専用のミラー値(#312)。AIを動かすのはホストだけ(#300)だが、ゲストも
    // ゴースト表示(#301)や決着判定のためにAIの盤面コピーを手元でシミュレートするので、
    // AI専用値もホストから配らないとゲスト側のコピーが別の地形になってしまう。
    // 反応速度系(move_cooldown_ms/dodge_recovery_ms)はAI専用値を持たない。
    pub ai_rock_spawn_rate_percent: u32,
    pub ai_air_spawn_rate_percent: u32,
    pub ai_star_spawn_rate_percent: u32,
    pub ai_diamond_spawn_rate_percent: u32,
    pub ai_item_clear_above_rate_percent: u32,
    pub ai_item_unify_colors_rate_percent: u32,
    pub ai_item_starify_screen_rate_percent: u32,
    pub ai_color_count: u8,
    pub ai_color_cluster_rate_percent: u32,
    pub ai_bomb_spawn_rate_percent: u32,
    pub ai_bomb_fuse_ms: u32,
    pub ai_attack_blocks_per_rock: u32,
    pub ai_attack_rocks_per_wave_max: u32,
    pub ai_attack_blocks_per_bomb: u32,
    pub ai_attack_bombs_per_wave_max: u32,
    pub ai_attack_bomb_ratio_percent: u32,
    pub ai_block_fall_tick_ms: u64,
    pub ai_player_fall_tick_ms: u64,
    pub ai_shake_duration_ms: u64,
    pub ai_chain_vanish_interval_ms: u64,
}

impl BattleConfig {
    /// ホスト側が対戦開始時に自分の設定から作る。`depth_goal_m`はモードセレクトで
    /// 選んだコースの値(呼び出し元から渡す。`Settings`が持つ`last_course_depth_m`は
    /// 次回起動時の初期選択を引き継ぐための値であり、今回選んだコースとは限らない)。
    pub fn from_settings(settings: &Settings, depth_goal_m: usize) -> Self {
        Self {
            depth_goal_m: depth_goal_m as u32,
            field_width: settings.field_width as u32,
            rock_spawn_rate_percent: settings.rock_spawn_rate_percent,
            air_spawn_rate_percent: settings.air_spawn_rate_percent,
            star_spawn_rate_percent: settings.star_spawn_rate_percent,
            diamond_spawn_rate_percent: settings.diamond_spawn_rate_percent,
            item_clear_above_rate_percent: settings.item_clear_above_rate_percent,
            item_unify_colors_rate_percent: settings.item_unify_colors_rate_percent,
            item_starify_screen_rate_percent: settings.item_starify_screen_rate_percent,
            color_count: settings.color_count,
            color_cluster_rate_percent: settings.color_cluster_rate_percent,
            bomb_spawn_rate_percent: settings.bomb_spawn_rate_percent,
            bomb_fuse_ms: settings.bomb_fuse_ms,
            attack_blocks_per_rock: settings.attack_blocks_per_rock,
            attack_rocks_per_wave_max: settings.attack_rocks_per_wave_max,
            attack_blocks_per_bomb: settings.attack_blocks_per_bomb,
            attack_bombs_per_wave_max: settings.attack_bombs_per_wave_max,
            attack_bomb_ratio_percent: settings.attack_bomb_ratio_percent,
            block_fall_tick_ms: settings.block_fall_tick_ms,
            player_fall_tick_ms: settings.player_fall_tick_ms,
            shake_duration_ms: settings.shake_duration_ms,
            move_cooldown_ms: settings.move_cooldown_ms,
            dodge_recovery_ms: settings.dodge_recovery_ms,
            chain_vanish_interval_ms: settings.chain_vanish_interval_ms,
            ai_rock_spawn_rate_percent: settings.ai_rock_spawn_rate_percent,
            ai_air_spawn_rate_percent: settings.ai_air_spawn_rate_percent,
            ai_star_spawn_rate_percent: settings.ai_star_spawn_rate_percent,
            ai_diamond_spawn_rate_percent: settings.ai_diamond_spawn_rate_percent,
            ai_item_clear_above_rate_percent: settings.ai_item_clear_above_rate_percent,
            ai_item_unify_colors_rate_percent: settings.ai_item_unify_colors_rate_percent,
            ai_item_starify_screen_rate_percent: settings.ai_item_starify_screen_rate_percent,
            ai_color_count: settings.ai_color_count,
            ai_color_cluster_rate_percent: settings.ai_color_cluster_rate_percent,
            ai_bomb_spawn_rate_percent: settings.ai_bomb_spawn_rate_percent,
            ai_bomb_fuse_ms: settings.ai_bomb_fuse_ms,
            ai_attack_blocks_per_rock: settings.ai_attack_blocks_per_rock,
            ai_attack_rocks_per_wave_max: settings.ai_attack_rocks_per_wave_max,
            ai_attack_blocks_per_bomb: settings.ai_attack_blocks_per_bomb,
            ai_attack_bombs_per_wave_max: settings.ai_attack_bombs_per_wave_max,
            ai_attack_bomb_ratio_percent: settings.ai_attack_bomb_ratio_percent,
            ai_block_fall_tick_ms: settings.ai_block_fall_tick_ms,
            ai_player_fall_tick_ms: settings.ai_player_fall_tick_ms,
            ai_shake_duration_ms: settings.ai_shake_duration_ms,
            ai_chain_vanish_interval_ms: settings.ai_chain_vanish_interval_ms,
        }
    }

    /// AI専用値(#312)を人間用の位置へ移した複製を返す。AIの盤面を作るときに使う。
    /// Game組み立ての手順はAIでも同じなので、手順ごと複製して別経路にするのではなく
    /// 値だけ差し替えて既存の組み立てを通し、手順が二重管理にならないようにしている。
    pub fn with_ai_values(&self) -> Self {
        Self {
            rock_spawn_rate_percent: self.ai_rock_spawn_rate_percent,
            air_spawn_rate_percent: self.ai_air_spawn_rate_percent,
            star_spawn_rate_percent: self.ai_star_spawn_rate_percent,
            diamond_spawn_rate_percent: self.ai_diamond_spawn_rate_percent,
            item_clear_above_rate_percent: self.ai_item_clear_above_rate_percent,
            item_unify_colors_rate_percent: self.ai_item_unify_colors_rate_percent,
            item_starify_screen_rate_percent: self.ai_item_starify_screen_rate_percent,
            color_count: self.ai_color_count,
            color_cluster_rate_percent: self.ai_color_cluster_rate_percent,
            bomb_spawn_rate_percent: self.ai_bomb_spawn_rate_percent,
            bomb_fuse_ms: self.ai_bomb_fuse_ms,
            attack_blocks_per_rock: self.ai_attack_blocks_per_rock,
            attack_rocks_per_wave_max: self.ai_attack_rocks_per_wave_max,
            attack_blocks_per_bomb: self.ai_attack_blocks_per_bomb,
            attack_bombs_per_wave_max: self.ai_attack_bombs_per_wave_max,
            attack_bomb_ratio_percent: self.ai_attack_bomb_ratio_percent,
            block_fall_tick_ms: self.ai_block_fall_tick_ms,
            player_fall_tick_ms: self.ai_player_fall_tick_ms,
            shake_duration_ms: self.ai_shake_duration_ms,
            chain_vanish_interval_ms: self.ai_chain_vanish_interval_ms,
            // 盤面幅とゴール深度は全員同じでないと対戦が成立しない。反応速度系は
            // AI専用値を持たないため、いずれも共通の値をそのまま残す。
            ..*self
        }
    }
}

/// フレーミングのペイロード長を表すプレフィックスのバイト数(u32のビッグエンディアン)。
const LENGTH_PREFIX_BYTES: usize = 4;

/// 受信時に受け付けるペイロード長の上限(バイト)。長さプレフィックスはu32なので、
/// 相手が送ってきた値をそのまま確保サイズに使うと1件で最大約4.29GBを求められ、
/// メモリ不足で落とされる。UDP探索で見つけた相手へ自動接続する作りのため、
/// 壊れた相手や別実装が繋がる場合に備えてここで上限を設ける。
///
/// 実測では現行の最大メッセージが8人ぶんの`RoomRoster`で347バイト、`StartConfig`は
/// 全項目を型の最大値にしても253バイトのため、1MiBあれば対戦人数や設定項目が増えても
/// 足りる。上限に当たった時点でその接続は破棄されるので、余裕を大きく取っている。
const MAX_MESSAGE_PAYLOAD_BYTES: usize = 1024 * 1024;

/// `bincode`の設定。送受信の両側で同じ設定を使う必要があるため、この1箇所に閉じる。
fn bincode_config() -> bincode::config::Configuration {
    bincode::config::standard()
}

/// メッセージを1件書き込む(spec.md 12.2)。4バイトのビッグエンディアン長さプレフィックス
/// (u32、以降のペイロードのバイト数)+`bincode`でシリアライズしたペイロードの順で書く。
pub fn write_message<W: Write>(writer: &mut W, msg: &GameMessage) -> io::Result<()> {
    let payload = bincode::serde::encode_to_vec(msg, bincode_config())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let length = u32::try_from(payload.len())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&payload)?;
    Ok(())
}

/// メッセージを1件読み込む(`write_message`の逆)。長さプレフィックスは相手から届く値で
/// あり、そのまま確保サイズには使えないため、`MAX_MESSAGE_PAYLOAD_BYTES`を超えていれば
/// 確保する前に`InvalidData`で断る。デシリアライズに失敗した場合も`InvalidData`にする。
pub fn read_message<R: Read>(reader: &mut R) -> io::Result<GameMessage> {
    let mut length_bytes = [0u8; LENGTH_PREFIX_BYTES];
    reader.read_exact(&mut length_bytes)?;
    let length = u32::from_be_bytes(length_bytes) as usize;
    if length > MAX_MESSAGE_PAYLOAD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "ペイロード長{length}バイトが上限{MAX_MESSAGE_PAYLOAD_BYTES}バイトを超えている"
            ),
        ));
    }

    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload)?;

    let (msg, _) = bincode::serde::decode_from_slice(&payload, bincode_config())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    Ok(msg)
}

/// ハンドシェイクの結果。両ホストがこの値を使って対戦の初期状態を組み立てる
/// (`battle::new_game_from_battle_config`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeResult {
    pub opponent_name: String,
    pub config: BattleConfig,
    pub seed: u64,
    /// AIの枠ぶんのシード(#319)。`SeedAgree`で配られたものをそのまま持つ。
    /// AIがいない対戦では空。
    pub ai_seeds: Vec<u64>,
}

/// TCPサーバ役(ACCEPTした側)=ホストのハンドシェイク(spec.md 12.2シーケンス1〜3)。
/// 自分の設定・シードを相手に一方的に通知する。
///
/// タイムアウト処理(`INVITE_TIMEOUT_MS`等)は#256の範囲のためここでは行わず、
/// `TcpStream`の読み書きはブロッキングのままとする。
///
/// #276でロビーがN人対戦のルーム(`room`モジュール)経由に切り替わったため、本体からは
/// 呼ばれなくなった。2人ぶんのハンドシェイクの取り決めを確かめるテストからのみ使う。
#[allow(dead_code)]
pub fn run_host_handshake(
    stream: &mut TcpStream,
    my_name: &str,
    config: BattleConfig,
) -> io::Result<HandshakeResult> {
    // クライアントが先に送る`Hello`を受けてから自分の`Hello`を返す(双方が同時に
    // 受信待ちへ入って止まらないよう、送受信の順序をホストとクライアントで逆にする)。
    let opponent_name = match read_message(stream)? {
        GameMessage::Hello {
            name,
            protocol_version,
        } => {
            // 設定・シードを送る前に版を確かめる(#317)。
            check_protocol_version(protocol_version)?;
            name
        }
        other => return Err(unexpected_message("Hello", &other)),
    };
    write_message(
        stream,
        &GameMessage::Hello {
            name: my_name.to_string(),
            protocol_version: PROTOCOL_VERSION,
        },
    )?;

    write_message(stream, &GameMessage::StartConfig(Box::new(config)))?;

    // シードはホストがOS乱数から単独で決める(「どちらのシードを使うか」の合意
    // プロトコルを省略するための取り決め。spec.md 12.2ステップ3)。
    let seed: u64 = rand::rng().random();
    // 2人版にAI(#300)は混ざらないため、AIぶんのシード(#319)は常に空。
    write_message(
        stream,
        &GameMessage::SeedAgree {
            seed,
            ai_seeds: Vec::new(),
        },
    )?;

    Ok(HandshakeResult {
        opponent_name,
        config,
        seed,
        ai_seeds: Vec::new(),
    })
}

/// TCPクライアント役(INVITEした側)のハンドシェイク。ホストの設定・シードを
/// そのまま受け取って従う(受け取った設定は対戦セッション中のみ適用し、自分の
/// `settings.json`へは保存しない)。
///
/// `run_host_handshake`と同様、#276以降はテストからのみ使う。
#[allow(dead_code)]
pub fn run_client_handshake(stream: &mut TcpStream, my_name: &str) -> io::Result<HandshakeResult> {
    write_message(
        stream,
        &GameMessage::Hello {
            name: my_name.to_string(),
            protocol_version: PROTOCOL_VERSION,
        },
    )?;
    let opponent_name = match read_message(stream)? {
        GameMessage::Hello {
            name,
            protocol_version,
        } => {
            // ホストの設定・シードを読む前に版を確かめる(#317)。違う版の値を
            // そのまま読むと、解釈のずれた設定で対戦を始めてしまう。
            check_protocol_version(protocol_version)?;
            name
        }
        other => return Err(unexpected_message("Hello", &other)),
    };

    let config = match read_message(stream)? {
        GameMessage::StartConfig(config) => *config,
        other => return Err(unexpected_message("StartConfig", &other)),
    };

    let (seed, ai_seeds) = match read_message(stream)? {
        GameMessage::SeedAgree { seed, ai_seeds } => (seed, ai_seeds),
        other => return Err(unexpected_message("SeedAgree", &other)),
    };

    Ok(HandshakeResult {
        opponent_name,
        config,
        seed,
        ai_seeds,
    })
}

/// ハンドシェイクの途中で想定外のメッセージ種別を受信したときのエラー。
/// ルーム参加のやり取り(#275)でも同じ形のエラーにするため`pub(crate)`にしている。
pub(crate) fn unexpected_message(expected: &str, actual: &GameMessage) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{expected}を待っていたが{actual:?}を受信した"),
    )
}

/// 受け取った`Hello`の版が自分と同じかを確かめる(#317)。違えば`InvalidData`で返し、
/// 呼び出し元は対戦を始めずに接続を切る。
///
/// メッシュ接続の`Hello`(#275)でも同じ判定をするため`pub(crate)`にしている。
pub(crate) fn check_protocol_version(peer_version: u32) -> io::Result<()> {
    if peer_version == PROTOCOL_VERSION {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "プロトコルバージョンが異なります(自分{PROTOCOL_VERSION}・相手{peer_version})。同じ版の実行ファイルどうしで対戦してください"
        ),
    ))
}

/// 通信スレッドがメインループへ届けるイベント(#254)。
pub enum NetworkEvent {
    Message(GameMessage),
    /// 受信ループがエラー(相手の切断・デコード失敗等)で終了した。
    Disconnected,
}

/// 受信専用スレッドを立て、`stream`から届いたメッセージを`tx`へ流し続ける
/// (spec.md 12.7「専用スレッド+mpscでメインのゲームループをブロックしない」)。
///
/// `GameMessage::Bye`を受信した場合、そのメッセージ自体を`tx`へ送ってからスレッドを
/// 終了する(呼び出し側がBye受信を扱えるように)。読み込みエラーの場合は
/// `NetworkEvent::Disconnected`を送って終了する。受け手(`BattleState`)が先に落ちて
/// チャネルが閉じた場合も、送信できなくなった時点で終了する。
pub fn spawn_receiver_thread(
    mut stream: TcpStream,
    tx: mpsc::Sender<NetworkEvent>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        loop {
            match read_message(&mut stream) {
                Ok(msg) => {
                    let is_bye = msg == GameMessage::Bye;
                    if tx.send(NetworkEvent::Message(msg)).is_err() {
                        // 受け手(対戦状態)が先に落ちた。届け先が無いため終了する。
                        return;
                    }
                    if is_bye {
                        return;
                    }
                }
                Err(_) => {
                    let _ = tx.send(NetworkEvent::Disconnected);
                    return;
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// UDPブロードキャストによる自動探索(#256。spec.md 12.1)
// ---------------------------------------------------------------------------

/// 対戦相手探索(HELLO/INVITE/ACCEPT/DECLINE/BYE)に使うUDPポート。
pub const DISCOVERY_PORT: u16 = 39393;
/// ゲーム開始後の本接続に使うTCPポートの既定値。使用中なら1つずつ上へ空きを探す。
pub const DEFAULT_TCP_PORT: u16 = 39394;
/// 候補リストから自動的に除去するまでの、最終受信からの経過時間(ms)。
pub const DISCOVERY_TIMEOUT_MS: u64 = 5000;
/// INVITEを送ってから応答を待つ上限(ms)。
pub const INVITE_TIMEOUT_MS: u64 = 10000;
/// TCP接続の確立を待つ上限(ms)。
pub const TCP_CONNECT_TIMEOUT_MS: u64 = 3000;
/// HELLOを再送する間隔(ms)。
pub const HELLO_BROADCAST_INTERVAL_MS: u64 = 1000;
/// 同一ホストで自動発見できる最大プロセス数(#278)。環境変数によるポート指定
/// (`MDT_DISCOVERY_PORT`)が無い場合、`DISCOVERY_PORT`からこの数だけ連続する
/// ポートを探索範囲として使う(空いている最初のポートにbindし、範囲内の全ポート
/// へHELLO/BYEをブロードキャストする)。
///
/// 対戦の定員(`ROOM_MAX_PLAYERS`)と同じ8にしてあるのは、1台のマシンで定員ぶんの
/// プロセスを起動して動作確認できるようにするため(#311)。別のマシンとの対戦には
/// 関係しない。
pub const DISCOVERY_PORT_RANGE_COUNT: u16 = 8;

/// 探索パケットの固定長(バイト)。受信バッファの大きさとしても使うため、
/// 探索側(`discovery.rs`)から参照できるようにしている。
pub(crate) const DISCOVERY_PACKET_LEN: usize = 60;

/// プロトコル識別子(spec.md 12.1のmagic)。
const DISCOVERY_MAGIC: [u8; 4] = *b"MDT1";
/// 探索プロトコルの版。旧版の実装は存在しないため現在は1固定。
/// TCP接続後のメッセージの版(`PROTOCOL_VERSION`)とは別物なので名前で区別する(#317)。
const DISCOVERY_PROTOCOL_VERSION: u8 = 1;
/// 表示名フィールドの長さ(バイト)。超える場合は切り詰め、余りは0でパディングする。
/// 名前入力UI(#270)が入力中にこの上限で打ち止めにするため、モジュール外からも参照する。
pub(crate) const PLAYER_NAME_LEN: usize = 16;

/// 探索パケットの種別(spec.md 12.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    /// 生存/募集アナウンス。1秒間隔で送信し続ける。
    Hello,
    /// 対戦招待。
    Invite,
    /// 招待受諾。
    Accept,
    /// 招待拒否。
    Decline,
    /// 探索/募集からの離脱通知。
    Bye,
    /// ゲストからの開始要求(ホストの追認なしに即座に開始する。#293)。
    RequestStart,
}

impl PacketType {
    fn to_byte(self) -> u8 {
        match self {
            PacketType::Hello => 0x01,
            PacketType::Invite => 0x02,
            PacketType::Accept => 0x03,
            PacketType::Decline => 0x04,
            PacketType::Bye => 0x05,
            PacketType::RequestStart => 0x06,
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x01 => Some(PacketType::Hello),
            0x02 => Some(PacketType::Invite),
            0x03 => Some(PacketType::Accept),
            0x04 => Some(PacketType::Decline),
            0x05 => Some(PacketType::Bye),
            0x06 => Some(PacketType::RequestStart),
            _ => None,
        }
    }
}

/// UDPでやり取りする探索パケット(spec.md 12.1)。
///
/// `GameMessage`(TCP)は`bincode`でシリアライズするが、こちらは仕様のテーブル通りの
/// 固定長60バイト・リトルエンディアンで手書きエンコードする(相手の実装が起動直後の
/// 段階でやり取りするため、シリアライザの型表現に依存させない)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryPacket {
    pub packet_type: PacketType,
    pub sender_id: Uuid,
    /// HELLO/BYEでは全ゼロ(`Uuid::nil()`)。INVITE/ACCEPT/DECLINE/REQUEST_STARTでは
    /// 相手の`sender_id`。
    pub target_id: Uuid,
    pub player_name: String,
    /// このホストが対戦開始後にlistenするTCPポート。
    pub tcp_port: u16,
}

impl DiscoveryPacket {
    pub fn encode(&self) -> [u8; DISCOVERY_PACKET_LEN] {
        let mut bytes = [0u8; DISCOVERY_PACKET_LEN];
        bytes[0..4].copy_from_slice(&DISCOVERY_MAGIC);
        bytes[4] = self.packet_type.to_byte();
        bytes[5] = DISCOVERY_PROTOCOL_VERSION;
        bytes[6..22].copy_from_slice(self.sender_id.as_bytes());
        bytes[22..38].copy_from_slice(self.target_id.as_bytes());
        // 表示名は16バイトに収め、余りは0のまま(パディング)にする。
        let name = truncate_on_char_boundary(&self.player_name, PLAYER_NAME_LEN);
        bytes[38..38 + name.len()].copy_from_slice(name.as_bytes());
        bytes[54..56].copy_from_slice(&self.tcp_port.to_le_bytes());
        // reserved(56..60)は送信時は全0。
        bytes
    }

    /// 長さ不足・magic不一致・不明なpacket_typeなら`None`。`protocol_version`と
    /// `reserved`は受信時には見ない(spec.md 12.1「受信時は無視する」)。
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < DISCOVERY_PACKET_LEN || bytes[0..4] != DISCOVERY_MAGIC {
            return None;
        }
        let packet_type = PacketType::from_byte(bytes[4])?;
        let sender_id = Uuid::from_bytes(bytes[6..22].try_into().ok()?);
        let target_id = Uuid::from_bytes(bytes[22..38].try_into().ok()?);

        Some(Self {
            packet_type,
            sender_id,
            target_id,
            player_name: decode_player_name(&bytes[38..54]),
            tcp_port: u16::from_le_bytes([bytes[54], bytes[55]]),
        })
    }
}

/// UTF-8のchar境界を壊さずに`max_bytes`以内へ切り詰める(spec.md 12.7)。
/// 収まらない場合は、収まる最後の文字の手前までを返す。
fn truncate_on_char_boundary(name: &str, max_bytes: usize) -> &str {
    if name.len() <= max_bytes {
        return name;
    }
    let mut end = max_bytes;
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// 表示名フィールド(0パディング済み)を文字列へ戻す。相手の実装が壊れたバイト列を
/// 送ってきても落ちないよう、不正なUTF-8は置換文字にする。
fn decode_player_name(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// テスト用の`BattleConfig`。既定値のままだと「ホストの値が本当に相手へ渡ったか」を
    /// 取り違えるおそれがあるため、各フィールドに異なる値を入れる。
    fn test_config() -> BattleConfig {
        BattleConfig {
            depth_goal_m: 500,
            field_width: 14,
            rock_spawn_rate_percent: 120,
            air_spawn_rate_percent: 80,
            star_spawn_rate_percent: 60,
            diamond_spawn_rate_percent: 40,
            item_clear_above_rate_percent: 110,
            item_unify_colors_rate_percent: 90,
            item_starify_screen_rate_percent: 70,
            color_count: 3,
            color_cluster_rate_percent: 130,
            bomb_spawn_rate_percent: 150,
            bomb_fuse_ms: 2500,
            attack_blocks_per_rock: 6,
            attack_rocks_per_wave_max: 2,
            attack_blocks_per_bomb: 18,
            attack_bombs_per_wave_max: 3,
            attack_bomb_ratio_percent: 25,
            block_fall_tick_ms: 200,
            player_fall_tick_ms: 100,
            shake_duration_ms: 300,
            move_cooldown_ms: 40,
            dodge_recovery_ms: 500,
            chain_vanish_interval_ms: 150,
            // AI専用値(#312)も人間用とは別の値を入れ、取り違えを検出できるようにする。
            ai_rock_spawn_rate_percent: 121,
            ai_air_spawn_rate_percent: 81,
            ai_star_spawn_rate_percent: 61,
            ai_diamond_spawn_rate_percent: 41,
            ai_item_clear_above_rate_percent: 111,
            ai_item_unify_colors_rate_percent: 91,
            ai_item_starify_screen_rate_percent: 71,
            ai_color_count: 2,
            ai_color_cluster_rate_percent: 131,
            ai_bomb_spawn_rate_percent: 151,
            ai_bomb_fuse_ms: 2600,
            ai_attack_blocks_per_rock: 7,
            ai_attack_rocks_per_wave_max: 1,
            ai_attack_blocks_per_bomb: 19,
            ai_attack_bombs_per_wave_max: 4,
            ai_attack_bomb_ratio_percent: 26,
            ai_block_fall_tick_ms: 210,
            ai_player_fall_tick_ms: 110,
            ai_shake_duration_ms: 310,
            ai_chain_vanish_interval_ms: 160,
        }
    }

    /// 1件書いて1件読み戻し、同じ値が復元されることを確認する。
    fn assert_round_trips(msg: &GameMessage) {
        let mut buffer = Vec::new();
        write_message(&mut buffer, msg).unwrap();

        let restored = read_message(&mut buffer.as_slice()).unwrap();

        assert_eq!(&restored, msg);
    }

    #[test]
    fn every_message_kind_round_trips_through_the_framing() {
        assert_round_trips(&GameMessage::Hello {
            name: "ホリ・ススム".to_string(),
            protocol_version: PROTOCOL_VERSION,
        });
        assert_round_trips(&GameMessage::JoinRoom {
            name: "ホリ・ススム".to_string(),
            mesh_port: 39395,
            protocol_version: PROTOCOL_VERSION,
        });
        assert_round_trips(&GameMessage::RoomRoster {
            members: vec![
                RoomMember {
                    name: "主催者".to_string(),
                    mesh_addr: Some("127.0.0.1:39394".parse().unwrap()),
                },
                RoomMember {
                    name: "参加者".to_string(),
                    // IPv6の参加者が混じっても同じ並びで運べること。
                    mesh_addr: Some("[::1]:39395".parse().unwrap()),
                },
                RoomMember {
                    // #300: 接続先を持たないAIの枠も同じ並びで運べること。
                    name: "AI 1".to_string(),
                    mesh_addr: None,
                },
            ],
            your_index: 1,
        });
        assert_round_trips(&GameMessage::StartConfig(Box::new(test_config())));
        assert_round_trips(&GameMessage::SeedAgree {
            seed: 0xdead_beef_0123_4567,
            ai_seeds: Vec::new(),
        });
        // #319: AIぶんの個別シードを並べた形も同じフレーミングで運べること。
        assert_round_trips(&GameMessage::SeedAgree {
            seed: 0xdead_beef_0123_4567,
            ai_seeds: vec![1, 0xffff_ffff_ffff_ffff],
        });
        assert_round_trips(&GameMessage::Input {
            action: NetAction::Drill,
            proxy_for: None,
        });
        assert_round_trips(&GameMessage::Heartbeat);
        assert_round_trips(&GameMessage::Attack {
            rock_amount: 3,
            bomb_amount: 1,
            proxy_for: None,
        });
        assert_round_trips(&GameMessage::Result {
            reached_goal: true,
            proxy_for: None,
        });
        // #300: AIの代理送信(room内インデックス付き)も同じフレーミングで運べること。
        assert_round_trips(&GameMessage::Input {
            action: NetAction::MoveLeft,
            proxy_for: Some(3),
        });
        assert_round_trips(&GameMessage::Attack {
            rock_amount: 5,
            bomb_amount: 0,
            proxy_for: Some(2),
        });
        assert_round_trips(&GameMessage::Result {
            reached_goal: false,
            proxy_for: Some(1),
        });
        assert_round_trips(&GameMessage::Bye);
    }

    #[test]
    fn boxing_the_start_config_payload_keeps_the_encoded_bytes_unchanged() {
        // #312で`StartConfig`の中身を間接参照へ移した。直列化の結果が中身をそのまま
        // 書いたものと同じでなければ、この変更だけで通信の互換性が壊れる。
        let config = test_config();

        let boxed = bincode::serde::encode_to_vec(Box::new(config), bincode_config()).unwrap();
        let direct = bincode::serde::encode_to_vec(config, bincode_config()).unwrap();

        assert_eq!(boxed, direct);
    }

    #[test]
    fn the_length_prefix_is_four_big_endian_bytes_of_the_payload_length() {
        // フレーミングの取り決め(spec.md 12.2)そのものを確認する。相手の実装が
        // 変わってもこの前提だけは崩せない。
        let mut buffer = Vec::new();
        write_message(&mut buffer, &GameMessage::Bye).unwrap();

        let length = u32::from_be_bytes(buffer[..LENGTH_PREFIX_BYTES].try_into().unwrap()) as usize;
        assert_eq!(length, buffer.len() - LENGTH_PREFIX_BYTES);
    }

    #[test]
    fn messages_written_back_to_back_are_read_back_in_order() {
        // 続けて書いた複数のメッセージが、長さプレフィックスの区切りどおりに
        // 1件ずつ取り出せること。
        let first = GameMessage::Hello {
            name: "a".to_string(),
            protocol_version: PROTOCOL_VERSION,
        };
        let second = GameMessage::SeedAgree {
            seed: 7,
            ai_seeds: Vec::new(),
        };
        let third = GameMessage::Bye;

        let mut buffer = Vec::new();
        write_message(&mut buffer, &first).unwrap();
        write_message(&mut buffer, &second).unwrap();
        write_message(&mut buffer, &third).unwrap();

        let mut reader = buffer.as_slice();
        assert_eq!(read_message(&mut reader).unwrap(), first);
        assert_eq!(read_message(&mut reader).unwrap(), second);
        assert_eq!(read_message(&mut reader).unwrap(), third);
    }

    #[test]
    fn reading_from_an_empty_or_truncated_stream_fails() {
        // 長さプレフィックスすら無い場合と、プレフィックスの示す長さぶんの
        // ペイロードが届いていない場合は、いずれもio::Errorになる。
        assert!(read_message(&mut [].as_slice()).is_err());

        let mut truncated = Vec::new();
        write_message(
            &mut truncated,
            &GameMessage::SeedAgree {
                seed: 1,
                ai_seeds: Vec::new(),
            },
        )
        .unwrap();
        truncated.pop();
        assert!(read_message(&mut truncated.as_slice()).is_err());
    }

    #[test]
    fn a_payload_at_the_size_limit_is_still_read_back() {
        // 上限ちょうどのメッセージは通ること。名前の長さでペイロードを上限へ合わせる
        // (内訳は判別子1バイト+文字列長のvarint 5バイト+版のvarint 1バイト+名前本体。
        // 版が251以上になると版のvarintが伸びるため、その時はこの引き算も直す)。
        let msg = GameMessage::Hello {
            name: "x".repeat(MAX_MESSAGE_PAYLOAD_BYTES - 7),
            protocol_version: PROTOCOL_VERSION,
        };

        let mut buffer = Vec::new();
        write_message(&mut buffer, &msg).unwrap();
        assert_eq!(
            buffer.len() - LENGTH_PREFIX_BYTES,
            MAX_MESSAGE_PAYLOAD_BYTES,
            "上限ちょうどのペイロードになっていない"
        );

        assert_eq!(read_message(&mut buffer.as_slice()).unwrap(), msg);
    }

    #[test]
    fn a_length_prefix_over_the_limit_is_rejected_before_allocating() {
        // 上限を超える長さプレフィックスだけを渡す。ペイロードは1バイトも用意しないので、
        // 確保前に断っていなければ読み込み側でUnexpectedEofになり種別が変わる。
        for length in [
            u32::try_from(MAX_MESSAGE_PAYLOAD_BYTES + 1).unwrap(),
            u32::MAX,
        ] {
            let mut cursor = io::Cursor::new(length.to_be_bytes().to_vec());

            let err = read_message(&mut cursor).unwrap_err();

            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "length={length}");
            // 長さプレフィックスの4バイトを読んだところで止まっていること。
            assert_eq!(cursor.position(), LENGTH_PREFIX_BYTES as u64);
        }
    }

    #[test]
    fn a_corrupted_payload_is_reported_as_invalid_data() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &GameMessage::Bye).unwrap();
        // 列挙子の判別子を、どのバリアントにも対応しない値へ壊す。
        buffer[LENGTH_PREFIX_BYTES] = 0xff;

        let err = read_message(&mut buffer.as_slice()).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn net_action_converts_to_the_matching_input_action() {
        let pairs = [
            (NetAction::MoveLeft, InputAction::MoveLeft),
            (NetAction::MoveRight, InputAction::MoveRight),
            (NetAction::FaceUp, InputAction::FaceUp),
            (NetAction::FaceDown, InputAction::FaceDown),
            (NetAction::Drill, InputAction::Drill),
        ];
        for (net, input) in pairs {
            assert_eq!(Option::<InputAction>::from(net), Some(input));
            assert_eq!(Option::<NetAction>::from(input), Some(net));
        }
    }

    #[test]
    fn net_action_none_means_no_input_for_the_tick() {
        assert_eq!(Option::<InputAction>::from(NetAction::None), None);
    }

    #[test]
    fn input_actions_outside_the_five_battle_operations_are_not_sent() {
        // 対戦tickに含めない操作(spec.md 12.5)は送信用の値へ変換されない。
        for action in [
            InputAction::TogglePause,
            InputAction::Quit,
            InputAction::ToggleMusic,
            InputAction::ToggleSe,
            InputAction::DebugAddLife,
        ] {
            assert_eq!(Option::<NetAction>::from(action), None);
        }
    }

    /// ループバックTCPでホスト役とクライアント役を実際に接続し、双方の
    /// `HandshakeResult`を返す。ホスト役は別スレッドで走らせる。
    fn run_loopback_handshake(config: BattleConfig) -> (HandshakeResult, HandshakeResult) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let host = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            run_host_handshake(&mut stream, "host", config).unwrap()
        });

        let mut stream = TcpStream::connect(addr).unwrap();
        let client_result = run_client_handshake(&mut stream, "client").unwrap();
        let host_result = host.join().unwrap();

        (host_result, client_result)
    }

    #[test]
    fn the_handshake_leaves_both_sides_with_the_same_config_and_seed() {
        let config = test_config();

        let (host, client) = run_loopback_handshake(config);

        assert_eq!(
            host.config, config,
            "ホストは自分が送った設定をそのまま使う"
        );
        assert_eq!(client.config, config, "クライアントはホストの設定に従う");
        assert_eq!(host.seed, client.seed);
        // #319: 2人版にAIは混ざらないため、AIぶんのシードは双方とも空になる。
        assert!(host.ai_seeds.is_empty());
        assert_eq!(client.ai_seeds, host.ai_seeds);
    }

    #[test]
    fn the_handshake_exchanges_the_display_names() {
        let (host, client) = run_loopback_handshake(test_config());

        assert_eq!(host.opponent_name, "client");
        assert_eq!(client.opponent_name, "host");
    }

    #[test]
    fn the_host_rejects_a_first_message_that_is_not_hello() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let host = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            run_host_handshake(&mut stream, "host", test_config())
        });

        let mut stream = TcpStream::connect(addr).unwrap();
        write_message(&mut stream, &GameMessage::Bye).unwrap();

        let err = host.join().unwrap().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn the_client_rejects_a_reply_that_is_not_hello() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        // ホストの振りをして、`Hello`の代わりに`Bye`を返す。
        let fake_host = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_message(&mut stream).unwrap();
            write_message(&mut stream, &GameMessage::Bye).unwrap();
        });

        let mut stream = TcpStream::connect(addr).unwrap();
        let err = run_client_handshake(&mut stream, "client").unwrap_err();
        fake_host.join().unwrap();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn the_client_rejects_a_config_step_that_is_not_start_config() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        // `Hello`までは正規の手順を踏み、`StartConfig`の代わりに`SeedAgree`を送る。
        let fake_host = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_message(&mut stream).unwrap();
            write_message(
                &mut stream,
                &GameMessage::Hello {
                    name: "host".to_string(),
                    protocol_version: PROTOCOL_VERSION,
                },
            )
            .unwrap();
            write_message(
                &mut stream,
                &GameMessage::SeedAgree {
                    seed: 1,
                    ai_seeds: Vec::new(),
                },
            )
            .unwrap();
        });

        let mut stream = TcpStream::connect(addr).unwrap();
        let err = run_client_handshake(&mut stream, "client").unwrap_err();
        fake_host.join().unwrap();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn the_host_rejects_a_hello_with_a_different_protocol_version() {
        // #317: 版が違う相手には設定・シードを送らずに切る。
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let host = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            run_host_handshake(&mut stream, "host", test_config())
        });

        let mut stream = TcpStream::connect(addr).unwrap();
        write_message(
            &mut stream,
            &GameMessage::Hello {
                name: "client".to_string(),
                protocol_version: PROTOCOL_VERSION + 1,
            },
        )
        .unwrap();

        let err = host.join().unwrap().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        // 設定を受け取る前に切れていること(版の判定がハンドシェイクの最初にある証拠)。
        assert!(
            read_message(&mut stream).is_err(),
            "版が違う相手にはStartConfigを送らないはず"
        );
    }

    #[test]
    fn the_client_rejects_a_hello_with_a_different_protocol_version() {
        // #317: ホストの版が違う場合も、設定を読む前に断る。
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        // ホストの振りをして、版だけ違う`Hello`を返す。
        let fake_host = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_message(&mut stream).unwrap();
            write_message(
                &mut stream,
                &GameMessage::Hello {
                    name: "host".to_string(),
                    protocol_version: PROTOCOL_VERSION + 1,
                },
            )
            .unwrap();
        });

        let mut stream = TcpStream::connect(addr).unwrap();
        let err = run_client_handshake(&mut stream, "client").unwrap_err();
        fake_host.join().unwrap();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn the_same_protocol_version_passes_the_check() {
        // 同じ版どうしなら通り、違えば`InvalidData`になること(判定そのものの確認)。
        assert!(check_protocol_version(PROTOCOL_VERSION).is_ok());

        let err = check_protocol_version(PROTOCOL_VERSION + 1).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn from_settings_copies_the_simulation_affecting_settings() {
        let settings = Settings {
            field_width: 9,
            rock_spawn_rate_percent: 120,
            air_spawn_rate_percent: 80,
            star_spawn_rate_percent: 60,
            diamond_spawn_rate_percent: 40,
            item_clear_above_rate_percent: 110,
            item_unify_colors_rate_percent: 90,
            item_starify_screen_rate_percent: 70,
            color_count: 3,
            color_cluster_rate_percent: 130,
            bomb_spawn_rate_percent: 150,
            bomb_fuse_ms: 2500,
            attack_blocks_per_rock: 6,
            attack_rocks_per_wave_max: 2,
            attack_blocks_per_bomb: 18,
            attack_bombs_per_wave_max: 3,
            attack_bomb_ratio_percent: 25,
            block_fall_tick_ms: 200,
            player_fall_tick_ms: 100,
            shake_duration_ms: 300,
            move_cooldown_ms: 40,
            dodge_recovery_ms: 500,
            chain_vanish_interval_ms: 150,
            ai_rock_spawn_rate_percent: 20,
            ai_air_spawn_rate_percent: 180,
            ai_star_spawn_rate_percent: 160,
            ai_diamond_spawn_rate_percent: 140,
            ai_item_clear_above_rate_percent: 10,
            ai_item_unify_colors_rate_percent: 190,
            ai_item_starify_screen_rate_percent: 170,
            ai_color_count: 1,
            ai_color_cluster_rate_percent: 30,
            ai_bomb_spawn_rate_percent: 50,
            ai_bomb_fuse_ms: 3500,
            ai_attack_blocks_per_rock: 16,
            ai_attack_rocks_per_wave_max: 5,
            ai_attack_blocks_per_bomb: 28,
            ai_attack_bombs_per_wave_max: 1,
            ai_attack_bomb_ratio_percent: 75,
            ai_block_fall_tick_ms: 400,
            ai_player_fall_tick_ms: 350,
            ai_shake_duration_ms: 700,
            ai_chain_vanish_interval_ms: 900,
            ..Default::default()
        };

        let config = BattleConfig::from_settings(&settings, 1000);

        assert_eq!(
            config,
            BattleConfig {
                depth_goal_m: 1000,
                field_width: 9,
                rock_spawn_rate_percent: 120,
                air_spawn_rate_percent: 80,
                star_spawn_rate_percent: 60,
                diamond_spawn_rate_percent: 40,
                item_clear_above_rate_percent: 110,
                item_unify_colors_rate_percent: 90,
                item_starify_screen_rate_percent: 70,
                color_count: 3,
                color_cluster_rate_percent: 130,
                bomb_spawn_rate_percent: 150,
                bomb_fuse_ms: 2500,
                attack_blocks_per_rock: 6,
                attack_rocks_per_wave_max: 2,
                attack_blocks_per_bomb: 18,
                attack_bombs_per_wave_max: 3,
                attack_bomb_ratio_percent: 25,
                block_fall_tick_ms: 200,
                player_fall_tick_ms: 100,
                shake_duration_ms: 300,
                move_cooldown_ms: 40,
                dodge_recovery_ms: 500,
                chain_vanish_interval_ms: 150,
                ai_rock_spawn_rate_percent: 20,
                ai_air_spawn_rate_percent: 180,
                ai_star_spawn_rate_percent: 160,
                ai_diamond_spawn_rate_percent: 140,
                ai_item_clear_above_rate_percent: 10,
                ai_item_unify_colors_rate_percent: 190,
                ai_item_starify_screen_rate_percent: 170,
                ai_color_count: 1,
                ai_color_cluster_rate_percent: 30,
                ai_bomb_spawn_rate_percent: 50,
                ai_bomb_fuse_ms: 3500,
                ai_attack_blocks_per_rock: 16,
                ai_attack_rocks_per_wave_max: 5,
                ai_attack_blocks_per_bomb: 28,
                ai_attack_bombs_per_wave_max: 1,
                ai_attack_bomb_ratio_percent: 75,
                ai_block_fall_tick_ms: 400,
                ai_player_fall_tick_ms: 350,
                ai_shake_duration_ms: 700,
                ai_chain_vanish_interval_ms: 900,
            }
        );
    }

    #[test]
    fn with_ai_values_replaces_only_the_twenty_mirrored_values() {
        // #312: AI用のGameを組み立てるときに、20項目がAI専用値へ入れ替わり、
        // 盤面幅・ゴール深度・反応速度系は共通の値のまま残ることを確認する。
        let config = test_config();

        let ai = config.with_ai_values();

        assert_eq!(
            ai.rock_spawn_rate_percent,
            config.ai_rock_spawn_rate_percent
        );
        assert_eq!(ai.air_spawn_rate_percent, config.ai_air_spawn_rate_percent);
        assert_eq!(
            ai.star_spawn_rate_percent,
            config.ai_star_spawn_rate_percent
        );
        assert_eq!(
            ai.diamond_spawn_rate_percent,
            config.ai_diamond_spawn_rate_percent
        );
        assert_eq!(
            ai.item_clear_above_rate_percent,
            config.ai_item_clear_above_rate_percent
        );
        assert_eq!(
            ai.item_unify_colors_rate_percent,
            config.ai_item_unify_colors_rate_percent
        );
        assert_eq!(
            ai.item_starify_screen_rate_percent,
            config.ai_item_starify_screen_rate_percent
        );
        assert_eq!(ai.color_count, config.ai_color_count);
        assert_eq!(
            ai.color_cluster_rate_percent,
            config.ai_color_cluster_rate_percent
        );
        assert_eq!(
            ai.bomb_spawn_rate_percent,
            config.ai_bomb_spawn_rate_percent
        );
        assert_eq!(ai.bomb_fuse_ms, config.ai_bomb_fuse_ms);
        assert_eq!(ai.attack_blocks_per_rock, config.ai_attack_blocks_per_rock);
        assert_eq!(
            ai.attack_rocks_per_wave_max,
            config.ai_attack_rocks_per_wave_max
        );
        assert_eq!(ai.attack_blocks_per_bomb, config.ai_attack_blocks_per_bomb);
        assert_eq!(
            ai.attack_bombs_per_wave_max,
            config.ai_attack_bombs_per_wave_max
        );
        assert_eq!(
            ai.attack_bomb_ratio_percent,
            config.ai_attack_bomb_ratio_percent
        );
        assert_eq!(ai.block_fall_tick_ms, config.ai_block_fall_tick_ms);
        assert_eq!(ai.player_fall_tick_ms, config.ai_player_fall_tick_ms);
        assert_eq!(ai.shake_duration_ms, config.ai_shake_duration_ms);
        assert_eq!(
            ai.chain_vanish_interval_ms,
            config.ai_chain_vanish_interval_ms
        );

        assert_eq!(ai.depth_goal_m, config.depth_goal_m);
        assert_eq!(ai.field_width, config.field_width);
        assert_eq!(ai.move_cooldown_ms, config.move_cooldown_ms);
        assert_eq!(ai.dodge_recovery_ms, config.dodge_recovery_ms);
    }

    /// ループバックTCPで1組の接続を作り、`(受信スレッドへ渡す側, 送りつける側)`を返す。
    fn loopback_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        (server, client)
    }

    /// 受信スレッドからのイベントを1件、上限時間まで待って取り出す。
    fn recv_event(rx: &mpsc::Receiver<NetworkEvent>) -> NetworkEvent {
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn the_receiver_thread_forwards_messages_in_order_and_stops_after_bye() {
        let (server, mut client) = loopback_pair();
        let (tx, rx) = mpsc::channel();
        let handle = spawn_receiver_thread(server, tx);

        write_message(&mut client, &GameMessage::Heartbeat).unwrap();
        write_message(
            &mut client,
            &GameMessage::Input {
                action: NetAction::Drill,
                proxy_for: None,
            },
        )
        .unwrap();
        write_message(&mut client, &GameMessage::Bye).unwrap();

        assert!(matches!(
            recv_event(&rx),
            NetworkEvent::Message(GameMessage::Heartbeat)
        ));
        assert!(matches!(
            recv_event(&rx),
            NetworkEvent::Message(GameMessage::Input {
                action: NetAction::Drill,
                proxy_for: None
            })
        ));
        assert!(
            matches!(recv_event(&rx), NetworkEvent::Message(GameMessage::Bye)),
            "Bye自体も呼び出し側へ届けてから終了するはず"
        );

        // Bye受信でスレッドが終わるため、以降のメッセージは届かない。検証対象は
        // 「届かないこと」自体であり、送信の成否は問わない(スレッド終了に伴う
        // TCP接続クローズのタイミングによっては、この書き込み自体がBrokenPipeで
        // 失敗することがある。#277)。
        handle.join().unwrap();
        let _ = write_message(&mut client, &GameMessage::Heartbeat);
        assert!(rx.recv().is_err(), "スレッド終了で送信側が閉じているはず");
    }

    #[test]
    fn the_receiver_thread_reports_a_closed_connection_as_disconnected() {
        let (server, client) = loopback_pair();
        let (tx, rx) = mpsc::channel();
        let handle = spawn_receiver_thread(server, tx);

        drop(client);

        assert!(matches!(recv_event(&rx), NetworkEvent::Disconnected));
        handle.join().unwrap();
    }

    #[test]
    fn from_settings_takes_the_goal_depth_from_the_argument_not_the_saved_course() {
        // `last_course_depth_m`は次回起動時の初期選択を引き継ぐための値で、
        // 今回選んだコースとは限らないため使わない。
        let settings = Settings {
            last_course_depth_m: 1000,
            ..Default::default()
        };

        assert_eq!(
            BattleConfig::from_settings(&settings, 500).depth_goal_m,
            500
        );
    }

    // -----------------------------------------------------------------------
    // UDP探索パケット(#256。spec.md 12.1)。
    // -----------------------------------------------------------------------

    /// 指定した種別のテスト用パケット。
    fn discovery_packet(packet_type: PacketType, target_id: Uuid) -> DiscoveryPacket {
        DiscoveryPacket {
            packet_type,
            sender_id: Uuid::from_u128(0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00),
            target_id,
            player_name: "Player-1a2b".to_string(),
            tcp_port: 39395,
        }
    }

    #[test]
    fn every_discovery_packet_kind_round_trips_through_the_fixed_layout() {
        for packet_type in [
            PacketType::Hello,
            PacketType::Invite,
            PacketType::Accept,
            PacketType::Decline,
            PacketType::Bye,
            PacketType::RequestStart,
        ] {
            // HELLO/BYEは全ゼロ、招待系は相手のIDを載せる。
            let target_id = match packet_type {
                PacketType::Hello | PacketType::Bye => Uuid::nil(),
                _ => Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10),
            };
            let packet = discovery_packet(packet_type, target_id);

            let restored = DiscoveryPacket::decode(&packet.encode()).unwrap();

            assert_eq!(restored, packet, "{packet_type:?}が復元できていない");
        }
    }

    #[test]
    fn the_encoded_packet_follows_the_byte_layout_in_the_spec() {
        // 相手の実装が変わってもこの並びだけは崩せない(spec.md 12.1のテーブル)。
        let packet = discovery_packet(PacketType::Invite, Uuid::from_u128(1));
        let bytes = packet.encode();

        assert_eq!(bytes.len(), 60);
        assert_eq!(&bytes[0..4], b"MDT1");
        assert_eq!(bytes[4], 0x02, "INVITEのpacket_typeは0x02");
        assert_eq!(bytes[5], 1, "protocol_versionは1固定");
        assert_eq!(&bytes[6..22], packet.sender_id.as_bytes());
        assert_eq!(&bytes[22..38], packet.target_id.as_bytes());
        assert_eq!(&bytes[38..49], b"Player-1a2b");
        assert_eq!(
            &bytes[49..54],
            &[0, 0, 0, 0, 0],
            "表示名の余りは0パディング"
        );
        assert_eq!(
            &bytes[54..56],
            &39395u16.to_le_bytes(),
            "tcp_portはリトルエンディアン"
        );
        assert_eq!(&bytes[56..60], &[0, 0, 0, 0], "reservedは送信時全0");
    }

    #[test]
    fn a_player_name_longer_than_the_field_is_truncated_at_a_char_boundary() {
        // 日本語(1文字3バイト)の表示名は16バイトに収まる5文字までで切れる。
        // 途中のバイトで切ると不正なUTF-8になるため、文字単位で切り詰める(spec.md 12.7)。
        let packet = DiscoveryPacket {
            player_name: "あいうえおかきくけこ".to_string(),
            ..discovery_packet(PacketType::Hello, Uuid::nil())
        };

        let restored = DiscoveryPacket::decode(&packet.encode()).unwrap();

        assert_eq!(restored.player_name, "あいうえお");
        assert_eq!(
            packet.encode()[53],
            0,
            "15バイトぶんしか使わないので末尾1バイトはパディングのはず"
        );
    }

    #[test]
    fn a_player_name_that_exactly_fills_the_field_is_kept_whole() {
        let packet = DiscoveryPacket {
            player_name: "0123456789abcdef".to_string(),
            ..discovery_packet(PacketType::Hello, Uuid::nil())
        };

        assert_eq!(
            DiscoveryPacket::decode(&packet.encode())
                .unwrap()
                .player_name,
            "0123456789abcdef"
        );
    }

    #[test]
    fn decoding_rejects_short_foreign_and_unknown_packets() {
        let packet = discovery_packet(PacketType::Hello, Uuid::nil());

        assert_eq!(
            DiscoveryPacket::decode(&packet.encode()[..59]),
            None,
            "60バイトに満たないパケットは受け付けない"
        );

        let mut foreign = packet.encode();
        foreign[0] = b'X';
        assert_eq!(
            DiscoveryPacket::decode(&foreign),
            None,
            "magicが違うパケットは他アプリのものとして無視する"
        );

        let mut unknown_kind = packet.encode();
        unknown_kind[4] = 0x09;
        assert_eq!(DiscoveryPacket::decode(&unknown_kind), None);
    }

    #[test]
    fn decoding_ignores_the_protocol_version_and_the_reserved_bytes() {
        // 受信時は無視する(spec.md 12.1)。将来の版のパケットでも候補として扱える。
        let packet = discovery_packet(PacketType::Hello, Uuid::nil());
        let mut bytes = packet.encode();
        bytes[5] = 9;
        bytes[56..60].copy_from_slice(&[1, 2, 3, 4]);

        assert_eq!(DiscoveryPacket::decode(&bytes), Some(packet));
    }
}
