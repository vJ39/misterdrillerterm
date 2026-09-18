//! 対戦のTCP層(#253。spec.md 12.2)。
//!
//! TCP接続が確立済みの2ホストが、メッセージのフレーミングを介してハンドシェイク
//! (Hello交換 → StartConfig → SeedAgree → StartCountdown)を行うところまでを担う。
//! UDP探索・招待ダイアログ(#256)、lockstepループ中の`Input`/`Heartbeat`/`StateHash`/
//! `Result`の継続送受信(#254/#255)は対象外で、それらのメッセージは型として定義だけして
//! おく。ハンドシェイクの結果から`Game`を組み立てるのはゲームロジック側の責務のため
//! `battle::new_game_from_battle_config`に置く。
//!
//! タイトルからの入口はまだ無く(#256)、検証はループバックTCP(`127.0.0.1:0`)を使った
//! ユニットテストで行う。

// このモジュールは#254で対戦画面へ配線するまでバイナリ側からは一切呼ばれず、ほぼ全ての
// 項目がdead_code警告の対象になる(`BattleState::new`等と同じ事情)。項目ごとに属性を
// 並べると読みづらいため、モジュール単位で抑止し、配線時にこの1行を外す。
#![allow(dead_code)]

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngExt;
use serde::{Deserialize, Serialize};

use crate::game::InputAction;
use crate::settings::Settings;

/// `StartCountdown`で指定する開始時刻を、ホストの現在時刻からどれだけ先にするか(ms)。
/// 両者はこの猶予の間に「3, 2, 1, GO」のカウントダウンをローカルの時計で独立表示する
/// (spec.md 12.2ステップ5)。
const START_COUNTDOWN_LEAD_MS: u64 = 3000;

/// TCP接続後にやり取りするメッセージ(spec.md 12.2)。
///
/// `Input`/`Heartbeat`/`StateHash`/`Result`は#253では送受信しない(#254/#255の範囲)が、
/// プロトコルとしては最初から完全な形で定義しておき、後から使う分だけ配線する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameMessage {
    Hello {
        name: String,
    },
    /// ホスト(TCPサーバ役)のシミュレーション影響設定一式。クライアントは
    /// この値を対戦セッション中のみ強制適用する(自分のsettings.jsonへは保存しない)。
    StartConfig(BattleConfig),
    SeedAgree {
        seed: u64,
    },
    StartCountdown {
        start_at_unix_ms: u64,
    },
    Input {
        tick: u32,
        action: NetAction,
    },
    Heartbeat {
        tick: u32,
    },
    /// 定期デシンク検出(spec.md 12.3)。互いのシミュレーション状態のダイジェストを照合する。
    StateHash {
        tick: u32,
        local_hash: u64,
        remote_hash: u64,
    },
    Result {
        reached_goal: bool,
        tick: u32,
        time_ms: u64,
    },
    Bye,
}

/// 1章の`InputAction`のうちネットワーク同期に必要な要素のみを送る(spec.md 12.2)。
/// TogglePause/Quit/ToggleMusic/ToggleSe/Debug*系はローカルのみで完結させ
/// (12.5の通り対戦中はほぼ無効化)、対戦tickには含めない。
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
    /// 受信した相手の入力を、そのtickで`lockstep::run_tick`へ渡す形へ変換する。
    /// `NetAction::None`は「このtickは何もしていない」を表すため`None`になる。
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
    pub block_fall_tick_ms: u64,
    pub player_fall_tick_ms: u64,
    pub shake_duration_ms: u64,
    pub move_cooldown_ms: u64,
    pub dodge_recovery_ms: u64,
    pub chain_vanish_interval_ms: u64,
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
            block_fall_tick_ms: settings.block_fall_tick_ms,
            player_fall_tick_ms: settings.player_fall_tick_ms,
            shake_duration_ms: settings.shake_duration_ms,
            move_cooldown_ms: settings.move_cooldown_ms,
            dodge_recovery_ms: settings.dodge_recovery_ms,
            chain_vanish_interval_ms: settings.chain_vanish_interval_ms,
        }
    }
}

/// フレーミングのペイロード長を表すプレフィックスのバイト数(u32のビッグエンディアン)。
const LENGTH_PREFIX_BYTES: usize = 4;

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

/// メッセージを1件読み込む(`write_message`の逆)。相手が同じ実装である前提のため、
/// 長さの妥当性チェックは行わない。デシリアライズに失敗した場合は`InvalidData`にする。
pub fn read_message<R: Read>(reader: &mut R) -> io::Result<GameMessage> {
    let mut length_bytes = [0u8; LENGTH_PREFIX_BYTES];
    reader.read_exact(&mut length_bytes)?;
    let length = u32::from_be_bytes(length_bytes) as usize;

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
    pub start_at_unix_ms: u64,
}

/// TCPサーバ役(ACCEPTした側)=ホストのハンドシェイク(spec.md 12.2シーケンス1〜3、5)。
/// 自分の設定・シード・開始時刻を相手に一方的に通知する。
///
/// タイムアウト処理(`INVITE_TIMEOUT_MS`等)は#256の範囲のためここでは行わず、
/// `TcpStream`の読み書きはブロッキングのままとする。
pub fn run_host_handshake(
    stream: &mut TcpStream,
    my_name: &str,
    config: BattleConfig,
) -> io::Result<HandshakeResult> {
    // クライアントが先に送る`Hello`を受けてから自分の`Hello`を返す(双方が同時に
    // 受信待ちへ入って止まらないよう、送受信の順序をホストとクライアントで逆にする)。
    let opponent_name = match read_message(stream)? {
        GameMessage::Hello { name } => name,
        other => return Err(unexpected_message("Hello", &other)),
    };
    write_message(
        stream,
        &GameMessage::Hello {
            name: my_name.to_string(),
        },
    )?;

    write_message(stream, &GameMessage::StartConfig(config))?;

    // シードはホストがOS乱数から単独で決める(「どちらのシードを使うか」の合意
    // プロトコルを省略するための取り決め。spec.md 12.2ステップ3)。
    let seed: u64 = rand::rng().random();
    write_message(stream, &GameMessage::SeedAgree { seed })?;

    let start_at_unix_ms = unix_time_ms().saturating_add(START_COUNTDOWN_LEAD_MS);
    write_message(stream, &GameMessage::StartCountdown { start_at_unix_ms })?;

    Ok(HandshakeResult {
        opponent_name,
        config,
        seed,
        start_at_unix_ms,
    })
}

/// TCPクライアント役(INVITEした側)のハンドシェイク。ホストの設定・シード・開始時刻を
/// そのまま受け取って従う(受け取った設定は対戦セッション中のみ適用し、自分の
/// `settings.json`へは保存しない)。
pub fn run_client_handshake(stream: &mut TcpStream, my_name: &str) -> io::Result<HandshakeResult> {
    write_message(
        stream,
        &GameMessage::Hello {
            name: my_name.to_string(),
        },
    )?;
    let opponent_name = match read_message(stream)? {
        GameMessage::Hello { name } => name,
        other => return Err(unexpected_message("Hello", &other)),
    };

    let config = match read_message(stream)? {
        GameMessage::StartConfig(config) => config,
        other => return Err(unexpected_message("StartConfig", &other)),
    };

    let seed = match read_message(stream)? {
        GameMessage::SeedAgree { seed } => seed,
        other => return Err(unexpected_message("SeedAgree", &other)),
    };

    let start_at_unix_ms = match read_message(stream)? {
        GameMessage::StartCountdown { start_at_unix_ms } => start_at_unix_ms,
        other => return Err(unexpected_message("StartCountdown", &other)),
    };

    Ok(HandshakeResult {
        opponent_name,
        config,
        seed,
        start_at_unix_ms,
    })
}

/// ハンドシェイクの途中で想定外のメッセージ種別を受信したときのエラー。
fn unexpected_message(expected: &str, actual: &GameMessage) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{expected}を待っていたが{actual:?}を受信した"),
    )
}

/// 現在のUNIX時刻(ms)。システム時計がUNIXエポックより前を指している場合は0を返す
/// (対戦開始時刻の共有はNTP的な厳密同期を前提にしていないため、ここでは失敗させない)。
fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
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
            block_fall_tick_ms: 200,
            player_fall_tick_ms: 100,
            shake_duration_ms: 300,
            move_cooldown_ms: 40,
            dodge_recovery_ms: 500,
            chain_vanish_interval_ms: 150,
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
        });
        assert_round_trips(&GameMessage::StartConfig(test_config()));
        assert_round_trips(&GameMessage::SeedAgree {
            seed: 0xdead_beef_0123_4567,
        });
        assert_round_trips(&GameMessage::StartCountdown {
            start_at_unix_ms: 1_700_000_000_000,
        });
        assert_round_trips(&GameMessage::Input {
            tick: 42,
            action: NetAction::Drill,
        });
        assert_round_trips(&GameMessage::Heartbeat { tick: 43 });
        assert_round_trips(&GameMessage::StateHash {
            tick: 60,
            local_hash: 1,
            remote_hash: 2,
        });
        assert_round_trips(&GameMessage::Result {
            reached_goal: true,
            tick: 1234,
            time_ms: 56_789,
        });
        assert_round_trips(&GameMessage::Bye);
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
        };
        let second = GameMessage::SeedAgree { seed: 7 };
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
        write_message(&mut truncated, &GameMessage::SeedAgree { seed: 1 }).unwrap();
        truncated.pop();
        assert!(read_message(&mut truncated.as_slice()).is_err());
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
    fn the_handshake_leaves_both_sides_with_the_same_config_seed_and_start_time() {
        let config = test_config();

        let (host, client) = run_loopback_handshake(config);

        assert_eq!(
            host.config, config,
            "ホストは自分が送った設定をそのまま使う"
        );
        assert_eq!(client.config, config, "クライアントはホストの設定に従う");
        assert_eq!(host.seed, client.seed);
        assert_eq!(host.start_at_unix_ms, client.start_at_unix_ms);
    }

    #[test]
    fn the_handshake_exchanges_the_display_names() {
        let (host, client) = run_loopback_handshake(test_config());

        assert_eq!(host.opponent_name, "client");
        assert_eq!(client.opponent_name, "host");
    }

    #[test]
    fn the_start_time_is_about_three_seconds_ahead_of_the_hosts_clock() {
        let before = unix_time_ms();
        let (host, _) = run_loopback_handshake(test_config());
        let after = unix_time_ms();

        assert!(
            host.start_at_unix_ms >= before + START_COUNTDOWN_LEAD_MS,
            "開始時刻はハンドシェイク開始時刻+3000ms以降のはず"
        );
        assert!(
            host.start_at_unix_ms <= after + START_COUNTDOWN_LEAD_MS,
            "開始時刻はハンドシェイク終了時刻+3000msを超えないはず"
        );
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
                },
            )
            .unwrap();
            write_message(&mut stream, &GameMessage::SeedAgree { seed: 1 }).unwrap();
        });

        let mut stream = TcpStream::connect(addr).unwrap();
        let err = run_client_handshake(&mut stream, "client").unwrap_err();
        fake_host.join().unwrap();

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
            block_fall_tick_ms: 200,
            player_fall_tick_ms: 100,
            shake_duration_ms: 300,
            move_cooldown_ms: 40,
            dodge_recovery_ms: 500,
            chain_vanish_interval_ms: 150,
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
                block_fall_tick_ms: 200,
                player_fall_tick_ms: 100,
                shake_duration_ms: 300,
                move_cooldown_ms: 40,
                dodge_recovery_ms: 500,
                chain_vanish_interval_ms: 150,
            }
        );
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
}
