//! First wire-format pieces for Minecraft Java 1.21.11 (protocol 774).
//!
//! Used by the Radio headless probe. The game connection still uses the
//! inherited 26.3 codecs.

use crate::{
    Result,
    buf::{Bool, Byte, Codec, Ident, Int, List, Long, Opt, Reader, UByte, VarInt, Writer},
    ids, packet,
    packets::{handshake, login, play},
};

/// Construct the initial 1.21.11 handshake packet without changing the
/// protocol number used by the inherited Horizon connection manager.
pub fn handshake_packet(host: impl Into<String>, port: u16, intent: i32) -> handshake::Serverbound {
    handshake::Serverbound::Intention(handshake::Intention {
        protocol: crate::radio_target::TARGET_PROTOCOL_VERSION,
        host: host.into(),
        port,
        intent,
    })
}

/// The 1.21.11 Login Success payload is just a game profile; the session
/// UUID was introduced later in 26.2 (protocol 776).
#[derive(Clone, Debug, PartialEq)]
pub struct LoginFinished12111 {
    pub profile: login::GameProfile,
}

impl Codec<LoginFinished12111> for LoginFinished12111 {
    fn read(r: &mut Reader) -> Result<LoginFinished12111> {
        Ok(Self {
            profile: <login::GameProfile as Codec<login::GameProfile>>::read(r)?,
        })
    }

    fn write(value: &LoginFinished12111, w: &mut Writer) {
        <login::GameProfile as Codec<login::GameProfile>>::write(&value.profile, w);
    }
}

/// Decode a 1.21.11 Login Success *payload* (after the packet ID), rejecting
/// extra bytes rather than silently treating a 26.3 payload as 1.21.11.
pub fn decode_login_finished(payload: &[u8]) -> Result<LoginFinished12111> {
    let mut reader = Reader::new(payload);
    let result = <LoginFinished12111 as Codec<LoginFinished12111>>::read(&mut reader)?;
    if reader.remaining() != 0 {
        return Err(crate::Error::new(
            "unexpected trailing bytes in 1.21.11 Login Success",
        ));
    }
    Ok(result)
}

packet!(
    /// The 1.21.11 spawn info uses byte game modes and -1 for no previous mode.
    SpawnInfo12111 {
        dimension_type: i32 => VarInt,
        dimension: String => Ident,
        seed: i64 => Long,
        game_type: u8 => UByte,
        previous_game_type: i8 => Byte,
        debug: bool => Bool,
        flat: bool => Bool,
        last_death: Option<play::GlobalPos> => Opt<play::GlobalPos>,
        portal_cooldown: i32 => VarInt,
        sea_level: i32 => VarInt,
    }
);

packet!(
    /// The first Play packet in 1.21.11, distinct from Login Success.
    PlayLogin12111 {
        player_id: i32 => Int,
        hardcore: bool => Bool,
        levels: Vec<String> => List<Ident>,
        max_players: i32 => VarInt,
        chunk_radius: i32 => VarInt,
        simulation_distance: i32 => VarInt,
        reduced_debug_info: bool => Bool,
        show_death_screen: bool => Bool,
        limited_crafting: bool => Bool,
        spawn: SpawnInfo12111 => SpawnInfo12111,
        enforces_secure_chat: bool => Bool,
    }
);

/// Decode the Play Login payload after removing its packet ID.
pub fn decode_play_login(payload: &[u8]) -> Result<PlayLogin12111> {
    let mut r = Reader::new(payload);
    let login = PlayLogin12111::read(&mut r)?;
    if r.remaining() != 0 {
        return Err(crate::Error::new(
            "unexpected trailing bytes in 1.21.11 Play Login",
        ));
    }
    Ok(login)
}

/// 1.21.11 acknowledges the teleport ID. Position and rotation follow in
/// a separate movement packet.
pub fn encode_teleport_ack(id: i32) -> Vec<u8> {
    let mut writer = Writer::new();
    // This packet has wire ID 0 in both versions.
    writer.varint(ids::play::serverbound::ACCEPT_TELEPORTATION);
    writer.varint(id);
    writer.data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_login_reads_byte_game_modes_and_one_chat_flag() {
        let mut payload = vec![
            0, 0, 0, 7, // entity ID
            0, 0, // hardcore, no listed dimensions
            20, 3, 3, // max players, view distance, simulation distance
            0, 1, 0, // reduced debug, death screen, limited crafting
            0, 19, // dimension type ID, dimension name length
        ];
        payload.extend_from_slice(b"minecraft:overworld");
        payload.extend_from_slice(&[
            0, 0, 0, 0, 0, 0, 0, 0, // seed
            1, 0xff, // creative, no previous game mode
            0, 1, 0, // debug, flat, no death position
            0, 63, 0, // portal cooldown, sea level, secure chat
        ]);
        let login = decode_play_login(&payload).unwrap();
        assert_eq!(login.player_id, 7);
        assert_eq!(login.spawn.dimension, "minecraft:overworld");
        assert_eq!(login.spawn.game_type, 1);
        assert_eq!(login.spawn.previous_game_type, -1);
        assert!(login.spawn.flat);
        assert!(decode_play_login(&payload[..payload.len() - 1]).is_err());
        payload.push(0);
        assert!(decode_play_login(&payload).is_err());
    }

    #[test]
    fn teleport_ack_contains_only_the_packet_and_teleport_ids() {
        assert_eq!(encode_teleport_ack(300), [0, 0xac, 0x02]);
    }

    #[test]
    fn handshake_uses_protocol_774() {
        let packet = handshake_packet("localhost", 25565, handshake::LOGIN);
        let bytes = packet.encode();
        assert_eq!(bytes[0], 0); // Handshake packet ID
        assert_eq!(&bytes[1..3], &[0x86, 0x06]); // VarInt 774
        crate::packets::round_trip(
            packet,
            handshake::Serverbound::encode,
            handshake::Serverbound::decode,
        );
    }

    #[test]
    fn handshake_supports_status_intent_without_changing_protocol() {
        let packet = handshake_packet("example.org", 25565, handshake::STATUS);
        let bytes = packet.encode();
        assert_eq!(bytes[0], 0);
        assert_eq!(&bytes[1..3], &[0x86, 0x06]);
        crate::packets::round_trip(
            packet,
            handshake::Serverbound::encode,
            handshake::Serverbound::decode,
        );
    }

    #[test]
    fn login_success_rejects_truncated_profiles() {
        let profile = login::GameProfile {
            uuid: 42,
            name: "RadioTest".into(),
            properties: vec![],
        };
        let mut writer = Writer::new();
        <login::GameProfile as Codec<login::GameProfile>>::write(&profile, &mut writer);
        for length in 0..writer.data.len() {
            assert!(
                decode_login_finished(&writer.data[..length]).is_err(),
                "truncated login profile unexpectedly decoded at {length} bytes"
            );
        }
    }

    #[test]
    fn login_success_has_no_session_uuid() {
        let profile = login::GameProfile {
            uuid: 42,
            name: "RadioTest".into(),
            properties: vec![],
        };
        let packet = LoginFinished12111 { profile };
        let mut writer = Writer::new();
        <LoginFinished12111 as Codec<LoginFinished12111>>::write(&packet, &mut writer);
        assert_eq!(decode_login_finished(&writer.data).unwrap(), packet);

        // The inherited 26.3 codec expects an additional 16-byte session UUID.
        assert!(
            <login::LoginFinished as Codec<login::LoginFinished>>::read(&mut Reader::new(
                &writer.data
            ))
            .is_err()
        );

        let mut with_session = writer.data;
        with_session.extend_from_slice(&[0; 16]);
        assert!(decode_login_finished(&with_session).is_err());
    }
}
