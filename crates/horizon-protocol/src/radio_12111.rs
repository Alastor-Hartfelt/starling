//! First wire-format pieces for Minecraft Java 1.21.11 (protocol 774).
//!
//! Used by the Radio headless probe. The game connection still uses the
//! inherited 26.3 codecs.

use crate::{
    Result,
    buf::{Bool, Byte, Codec, Ident, Int, List, Long, Opt, Reader, UByte, VarInt, Writer},
    chunk::{ChunkData, Layers, LightData, Paletted, Strategy},
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

impl From<SpawnInfo12111> for play::SpawnInfo {
    fn from(p: SpawnInfo12111) -> Self {
        Self {
            dimension_type: p.dimension_type,
            dimension: p.dimension,
            seed: p.seed,
            game_type: i32::from(p.game_type),
            previous_game_type: (p.previous_game_type >= 0)
                .then_some(i32::from(p.previous_game_type)),
            debug: p.debug,
            flat: p.flat,
            last_death: p.last_death,
            portal_cooldown: p.portal_cooldown,
            sea_level: p.sea_level,
        }
    }
}

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

packet!(
    /// Play Respawn uses the same byte game-mode encoding as Play Login.
    Respawn12111 {
        spawn: SpawnInfo12111 => SpawnInfo12111,
        keep: i8 => Byte,
    }
);

/// Decode the Play Respawn payload after removing its packet ID.
pub fn decode_respawn(payload: &[u8]) -> Result<Respawn12111> {
    let mut r = Reader::new(payload);
    let packet = Respawn12111::read(&mut r)?;
    if r.remaining() != 0 {
        return Err(crate::Error::new(
            "unexpected trailing bytes in 1.21.11 Play Respawn",
        ));
    }
    Ok(packet)
}

/// Decode a 1.21.11 Level Chunk With Light payload after removing its packet ID.
pub fn decode_level_chunk(payload: &[u8]) -> Result<play::LevelChunkWithLight> {
    let mut r = Reader::new(payload);
    let x = r.i32()?;
    let z = r.i32()?;
    let chunk = ChunkData::read(&mut r)?;
    // 1.21.11 sends BitSet.toLongArray(), with each long big-endian on the wire.
    // The shared LightData uses low-bit-first bytes for mask lookup.
    let read_mask = |r: &mut Reader| -> Result<Vec<u8>> {
        Ok(r.long_array()?
            .into_iter()
            .flat_map(i64::to_le_bytes)
            .collect())
    };
    let light = LightData {
        sky_mask: read_mask(&mut r)?,
        block_mask: read_mask(&mut r)?,
        empty_sky_mask: read_mask(&mut r)?,
        empty_block_mask: read_mask(&mut r)?,
        sky: Layers::read(&mut r)?,
        block: Layers::read(&mut r)?,
    };
    if r.remaining() != 0 {
        return Err(crate::Error::new(
            "unexpected trailing bytes in 1.21.11 chunk packet",
        ));
    }
    Ok(play::LevelChunkWithLight { x, z, chunk, light })
}

/// The 1.21.11 section header carries a block count followed by both palettes.
#[derive(Debug, PartialEq)]
pub struct ChunkSection12111 {
    pub non_empty: i16,
    pub states: Paletted,
    pub biomes: Paletted,
}

/// Decode the section buffer in a vanilla 1.21.11 chunk packet.
pub fn decode_chunk_sections(
    payload: &[u8],
    count: usize,
    biome_bits: u8,
) -> Result<Vec<ChunkSection12111>> {
    // The 1.21.11 vanilla block report has 29,671 global state IDs.
    const BLOCK_BITS: u8 = 15;
    // Each section needs at least two count bytes and two singleton palettes.
    if count > payload.len() / 6 {
        return Err(crate::Error::new("truncated 1.21.11 chunk section buffer"));
    }
    let mut r = Reader::new(payload);
    let mut sections = Vec::with_capacity(count);
    for _ in 0..count {
        let non_empty = r.i16()?;
        // Vanilla counts a non-air block and a non-empty fluid separately.
        if !(0..=8192).contains(&non_empty) {
            return Err(crate::Error::new(format!(
                "invalid 1.21.11 section block count {non_empty} at byte {}",
                r.position() - 2
            )));
        }
        sections.push(ChunkSection12111 {
            non_empty,
            states: Paletted::read(&mut r, Strategy::Blocks, BLOCK_BITS)?,
            biomes: Paletted::read(&mut r, Strategy::Biomes, biome_bits)?,
        });
    }
    if r.remaining() != 0 {
        return Err(crate::Error::new(
            "unexpected trailing bytes in 1.21.11 chunk sections",
        ));
    }
    Ok(sections)
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
    fn chunk_light_masks_decode_from_big_endian_longs() {
        let mut payload = vec![
            0xff, 0xff, 0xff, 0xfd, 0, 0, 0, 4, // chunk (-3, 4)
            0, 0, 0, // empty heightmaps, section data and block entities
            0, 0, // no populated sky or block light
            1, 0, 0, 0, 0, 0, 0, 1, 1, // one big-endian long; empty sky bits 0 and 8
            0, 0, 0, // no empty block light or light data arrays
        ];
        let packet = decode_level_chunk(&payload).unwrap();
        assert_eq!((packet.x, packet.z), (-3, 4));
        assert!(crate::buf::bit(&packet.light.empty_sky_mask, 0));
        assert!(crate::buf::bit(&packet.light.empty_sky_mask, 8));
        assert!(!crate::buf::bit(&packet.light.empty_sky_mask, 9));
        payload.push(0);
        assert!(decode_level_chunk(&payload).is_err());
    }

    #[test]
    fn section_header_has_one_block_count() {
        let payload = [0, 0, 0, 0, 0, 42];
        let sections = decode_chunk_sections(&payload, 1, 6).unwrap();
        assert_eq!(sections[0].non_empty, 0);
        assert_eq!(sections[0].states.get(4095), 0);
        assert_eq!(sections[0].biomes.get(63), 42);
        assert!(decode_chunk_sections(&payload, 2, 6).is_err());
        let fluid_filled = [0x20, 0, 0, 0, 0, 42];
        assert_eq!(
            decode_chunk_sections(&fluid_filled, 1, 6).unwrap()[0].non_empty,
            8192
        );

        let mut with_extra_count = payload.to_vec();
        with_extra_count.splice(2..2, [0, 0]);
        assert!(decode_chunk_sections(&with_extra_count, 1, 6).is_err());
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
