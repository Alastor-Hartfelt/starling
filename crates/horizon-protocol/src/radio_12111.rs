//! Wire formats for the experimental Minecraft Java 1.21.11 (protocol 774) session.

mod block_state_ranges;
mod entity_types;
mod item_types;

use crate::{
    Result,
    buf::{Bool, Byte, Codec, Ident, Int, List, Long, Opt, Reader, UByte, VarInt, Vec3d, Writer},
    chunk::{BLOCKS, ChunkData, Layers, LightData, Paletted, Strategy, pack},
    ids,
    item::{Component, Hashed, Stack, component},
    packet,
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

/// Translate a 1.21.11 spawn to the engine's entity registry. A falling
/// block also carries a block state ID in its spawn data.
pub fn remap_entity_spawn(mut spawn: play::AddEntity) -> Result<play::AddEntity> {
    if spawn.kind == entity_types::FALLING_BLOCK {
        spawn.data = block_state_to_engine(spawn.data)
            .ok_or_else(|| crate::Error::new("unknown 1.21.11 falling block state"))?;
    }
    spawn.kind = usize::try_from(spawn.kind)
        .ok()
        .and_then(|id| entity_types::ENTITY_TYPES.get(id).copied())
        .map(i32::from)
        .ok_or_else(|| crate::Error::new("unknown 1.21.11 entity type"))?;
    Ok(spawn)
}

/// 1.21.11 synchronizes one absolute position, before PositionPath was
/// introduced. Consume the old velocity field so the remaining fields align.
pub fn decode_entity_position_sync(payload: &[u8]) -> Result<play::EntityPositionSync> {
    let mut r = Reader::new(payload);
    let id = r.varint()?;
    let position = Vec3d::read(&mut r)?;
    let _velocity = Vec3d::read(&mut r)?;
    let packet = play::EntityPositionSync {
        id,
        path: play::PositionPath::Linear(position),
        yaw: r.f32()?,
        pitch: r.f32()?,
        on_ground: r.bool()?,
    };
    if r.remaining() != 0 {
        return Err(crate::Error::new(
            "unexpected trailing bytes in 1.21.11 entity position",
        ));
    }
    Ok(packet)
}

/// Decode a 1.21.11 relative entity move. Unlike 26.3, the position is
/// always three shorts, and the on-ground flag follows the optional rotation.
pub fn decode_entity_move(id: i32, payload: &[u8]) -> Result<play::Clientbound> {
    use crate::ids::play::clientbound as ids;
    let mut r = Reader::new(payload);
    let entity_id = r.varint()?;
    let delta = match id {
        ids::MOVE_ENTITY_POS | ids::MOVE_ENTITY_POS_ROT => {
            Some(play::VecDelta::Linear([r.i16()?, r.i16()?, r.i16()?]))
        }
        ids::MOVE_ENTITY_ROT => None,
        _ => return Err(crate::Error::new("not a 1.21.11 entity move packet")),
    };
    let rotation = if id == ids::MOVE_ENTITY_POS {
        None
    } else {
        Some((r.i8()?, r.i8()?))
    };
    let movement = play::MoveEntity {
        id: entity_id,
        delta,
        rotation,
        on_ground: r.bool()?,
    };
    if r.remaining() != 0 {
        return Err(crate::Error::new(
            "unexpected trailing bytes in 1.21.11 entity move",
        ));
    }
    Ok(match id {
        ids::MOVE_ENTITY_POS => play::Clientbound::MoveEntityPos(play::MoveEntityPos(movement)),
        ids::MOVE_ENTITY_POS_ROT => {
            play::Clientbound::MoveEntityPosRot(play::MoveEntityPosRot(movement))
        }
        _ => play::Clientbound::MoveEntityRot(play::MoveEntityRot(movement)),
    })
}

/// Map a 1.21.11 block entity type ID to its 26.3 counterpart. Beds no
/// longer have a separate block entity in the target registry.
pub fn block_entity_type_to_engine(id: i32) -> Option<i32> {
    usize::try_from(id)
        .ok()
        .and_then(|id| entity_types::BLOCK_ENTITY_TYPES.get(id).copied().flatten())
        .map(i32::from)
}

/// Decode a 1.21.11 Level Chunk With Light payload after removing its packet ID.
pub fn decode_level_chunk(payload: &[u8]) -> Result<play::LevelChunkWithLight> {
    let mut r = Reader::new(payload);
    let x = r.i32()?;
    let z = r.i32()?;
    let mut chunk = ChunkData::read(&mut r)?;
    chunk.block_entities.retain_mut(|entity| {
        if let Some(kind) = block_entity_type_to_engine(entity.kind) {
            entity.kind = kind;
            true
        } else {
            false
        }
    });
    let light = read_light_data(&mut r)?;
    if r.remaining() != 0 {
        return Err(crate::Error::new(
            "unexpected trailing bytes in 1.21.11 chunk packet",
        ));
    }
    Ok(play::LevelChunkWithLight { x, z, chunk, light })
}

/// Decode an incremental 1.21.11 light update.
pub fn decode_light_update(payload: &[u8]) -> Result<play::LightUpdate> {
    let mut r = Reader::new(payload);
    let x = r.varint()?;
    let z = r.varint()?;
    let light = read_light_data(&mut r)?;
    if r.remaining() != 0 {
        return Err(crate::Error::new(
            "unexpected trailing bytes in 1.21.11 light update",
        ));
    }
    Ok(play::LightUpdate { x, z, light })
}

fn read_light_data(r: &mut Reader) -> Result<LightData> {
    // 1.21.11 sends BitSet.toLongArray(), with each long big-endian on the wire.
    // The shared LightData uses low-bit-first bytes for mask lookup.
    let read_mask = |r: &mut Reader| -> Result<Vec<u8>> {
        Ok(r.long_array()?
            .into_iter()
            .flat_map(i64::to_le_bytes)
            .collect())
    };
    let light = LightData {
        sky_mask: read_mask(r)?,
        block_mask: read_mask(r)?,
        empty_sky_mask: read_mask(r)?,
        empty_block_mask: read_mask(r)?,
        sky: Layers::read(r)?,
        block: Layers::read(r)?,
    };
    if light
        .sky
        .iter()
        .chain(&light.block)
        .any(|layer| layer.len() != 2048)
    {
        return Err(crate::Error::new("invalid 1.21.11 light layer length"));
    }
    Ok(light)
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
            states: Paletted::read(&mut r, Strategy::Blocks, block_state_ranges::SOURCE_BITS)?,
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

// The generated runs are expanded at compile time. Decoding a direct palette
// needs a fixed-size table lookup per block, even when the IDs are reordered.
static BLOCK_STATES: [u16; block_state_ranges::SOURCE_LEN] = {
    use block_state_ranges::{SOURCE_LEN, STARTS};
    let mut table = [0; SOURCE_LEN];
    let mut run = 0;
    let mut id = 0;
    while id < SOURCE_LEN {
        if run + 1 < STARTS.len() && id == STARTS[run + 1].0 as usize {
            run += 1;
        }
        let (source, target) = STARTS[run];
        table[id] = target + (id as u16 - source);
        id += 1;
    }
    table
};

/// The exact 26.3 block state corresponding to a vanilla 1.21.11 state ID.
pub fn block_state_to_engine(id: i32) -> Option<i32> {
    usize::try_from(id)
        .ok()
        .and_then(|i| BLOCK_STATES.get(i).copied())
        .map(i32::from)
}

/// Translate a decoded block palette into 26.3 IDs. Indirect palettes keep
/// their existing indices. Direct palettes are repacked at the engine width.
pub fn remap_block_palette(mut blocks: Paletted) -> Result<Paletted> {
    let remap = |id| {
        block_state_to_engine(id)
            .ok_or_else(|| crate::Error::new(format!("unknown 1.21.11 block state ID {id}")))
    };
    if blocks.palette.is_empty() {
        if blocks.bits != block_state_ranges::SOURCE_BITS
            || blocks.data.len() != crate::chunk::storage_len(blocks.bits, BLOCKS)
        {
            return Err(crate::Error::new("invalid 1.21.11 direct block palette"));
        }
        let values = (0..BLOCKS)
            .map(|index| remap(blocks.get(index)))
            .collect::<Result<Vec<_>>>()?;
        blocks.bits = block_state_ranges::TARGET_BITS;
        blocks.data = pack(&values, blocks.bits, |&id| id as u64);
    } else {
        for id in &mut blocks.palette {
            *id = remap(*id)?;
        }
    }
    Ok(blocks)
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

fn item_registry_id(id: i32, lookup: &[i16]) -> Result<i32> {
    usize::try_from(id)
        .ok()
        .and_then(|id| lookup.get(id).copied())
        .filter(|&id| id >= 0)
        .map(i32::from)
        .ok_or_else(|| crate::Error::new(format!("unmapped 1.21.11 item registry ID {id}")))
}

fn read_item_slot(reader: &mut Reader) -> Result<Option<Stack>> {
    let count = reader.varint()?;
    if count <= 0 {
        return Ok(None);
    }
    let item = item_registry_id(reader.varint()?, &item_types::ITEMS)?;
    let mut stack = Stack::new(item, count);
    let added = reader.count(104)?;
    let removed = reader.count(104)?;
    for _ in 0..added {
        let kind = item_registry_id(reader.varint()?, &item_types::COMPONENTS)?;
        match kind {
            component::CUSTOM_DATA
            | component::MAX_STACK_SIZE
            | component::MAX_DAMAGE
            | component::DAMAGE
            | component::UNBREAKABLE
            | component::CUSTOM_NAME
            | component::ITEM_NAME
            | component::ITEM_MODEL
            | component::LORE
            | component::RARITY
            | component::REPAIR_COST
            | component::ENCHANTMENT_GLINT_OVERRIDE => {}
            // 1.21.11 does not include per-component lengths here. Refuse the
            // packet if the component's wire format has not been checked.
            _ => {
                return Err(crate::Error::new(format!(
                    "unsupported 1.21.11 item component {kind}"
                )));
            }
        }
        stack.added.push((kind, Component::read(kind, reader)?));
    }
    for _ in 0..removed {
        stack
            .removed
            .push(item_registry_id(reader.varint()?, &item_types::COMPONENTS)?);
    }
    Ok(Some(stack))
}

pub fn decode_inventory(id: i32, payload: &[u8]) -> Result<play::Clientbound> {
    use ids::play::clientbound as canonical;
    let mut reader = Reader::new(payload);
    let packet = match id {
        canonical::CONTAINER_SET_CONTENT => {
            let container = reader.varint()?;
            let state = reader.varint()?;
            let count = reader.count(reader.remaining())?;
            let items = (0..count)
                .map(|_| read_item_slot(&mut reader))
                .collect::<Result<_>>()?;
            play::Clientbound::ContainerSetContent(play::ContainerSetContent {
                container,
                state,
                items,
                carried: read_item_slot(&mut reader)?,
            })
        }
        canonical::CONTAINER_SET_SLOT => {
            play::Clientbound::ContainerSetSlot(play::ContainerSetSlot {
                container: reader.varint()?,
                state: reader.varint()?,
                slot: reader.i16()?,
                item: read_item_slot(&mut reader)?,
            })
        }
        canonical::SET_PLAYER_INVENTORY => {
            play::Clientbound::SetPlayerInventory(play::SetPlayerInventory {
                slot: reader.varint()?,
                item: read_item_slot(&mut reader)?,
            })
        }
        canonical::SET_CURSOR_ITEM => play::Clientbound::SetCursorItem(play::SetCursorItem {
            item: read_item_slot(&mut reader)?,
        }),
        _ => return Err(crate::Error::new("unknown 1.21.11 inventory packet")),
    };
    if reader.remaining() != 0 {
        return Err(crate::Error::new(
            "trailing bytes in 1.21.11 inventory packet",
        ));
    }
    Ok(packet)
}

fn legacy_registry_id(id: i32, lookup: &[i16]) -> Option<i32> {
    let id = i16::try_from(id).ok().filter(|&id| id >= 0)?;
    lookup
        .iter()
        .position(|&value| value == id)
        .map(|index| index as i32)
}

fn remap_slot_hash(slot: &mut Option<Hashed>) -> bool {
    let Some(stack) = slot else {
        return true;
    };
    let Some(item) = legacy_registry_id(stack.item, &item_types::ITEMS) else {
        return false;
    };
    stack.item = item;
    stack.added.retain_mut(|(id, _)| {
        legacy_registry_id(*id, &item_types::COMPONENTS).is_some_and(|legacy| {
            *id = legacy;
            true
        })
    });
    stack.removed.retain_mut(|id| {
        legacy_registry_id(*id, &item_types::COMPONENTS).is_some_and(|legacy| {
            *id = legacy;
            true
        })
    });
    true
}

/// The 1.21.11 click layout uses the same prediction hashes and older registry
/// IDs. Omitted predictions are synchronized from the authoritative menu.
pub fn remap_container_click(mut click: play::ContainerClick) -> play::ContainerClick {
    click
        .changed
        .retain_mut(|(_, prediction)| remap_slot_hash(prediction));
    if !remap_slot_hash(&mut click.carried) {
        click.carried = None;
    }
    click
}

/// 26.3 added ChangeDestroyDirection after StartDestroyBlock. The remaining
/// ordinals shift back by one on 1.21.11; the rest of the payload is identical.
pub fn remap_player_action(mut action: play::PlayerAction) -> Option<play::PlayerAction> {
    action.action = match action.action {
        0 => 0,
        2..=8 => action.action - 1,
        _ => return None,
    };
    Some(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_click_translates_predicted_stack_ids_back_to_12111() {
        let click = play::ContainerClick {
            container: 1,
            state: 2,
            slot: 0,
            button: 0,
            input: 1,
            changed: vec![(0, None), (54, Some(Hashed::of(&Stack::new(128, 2))))],
            carried: None,
        };
        let clicked = remap_container_click(click);
        assert!(clicked.changed[0].1.is_none());
        let stack = clicked.changed[1].1.as_ref().unwrap();
        assert_eq!((stack.item, stack.count), (92, 2));
    }

    #[test]
    fn translates_a_12111_hotbar_slot_with_damage() {
        // Slot 3, count 1, 1.21.11 golden pickaxe (928), one added
        // component, zero removed, minecraft:damage (3), damage value 11.
        let payload = [3, 1, 0xa0, 7, 1, 0, 3, 11];
        let kind = ids::play::clientbound::SET_PLAYER_INVENTORY;
        let play::Clientbound::SetPlayerInventory(packet) =
            decode_inventory(kind, &payload).unwrap()
        else {
            panic!("expected player inventory update");
        };
        let stack = packet.item.unwrap();
        assert_eq!((packet.slot, stack.item, stack.damage()), (3, 1042, 11));
        assert!(decode_inventory(kind, &payload[..7]).is_err());
        // Component 104 is out of bounds and its unframed value cannot be skipped.
        assert!(decode_inventory(kind, &[3, 1, 0xa0, 7, 1, 0, 104, 11]).is_err());
    }

    #[test]
    fn actions_use_the_12111_ordinals() {
        let expected = [
            Some(0),
            None,
            Some(1),
            Some(2),
            Some(3),
            Some(4),
            Some(5),
            Some(6),
            Some(7),
            None,
        ];
        for (action, expected) in expected.into_iter().enumerate() {
            let packet = play::PlayerAction {
                action: action as i32,
                pos: [-1, 70, 2],
                direction: 1,
                sequence: 39,
            };
            let mapped = remap_player_action(packet.clone());
            assert_eq!(
                mapped,
                expected.map(|action| play::PlayerAction { action, ..packet })
            );
        }
    }

    #[test]
    fn block_entity_ids_are_translated_and_retired_beds_have_no_engine_type() {
        assert_eq!(block_entity_type_to_engine(27), Some(26)); // Barrel.
        assert_eq!(block_entity_type_to_engine(25), None); // Bed.
        assert_eq!(block_entity_type_to_engine(-1), None);
    }

    #[test]
    fn relative_entity_move_has_fixed_deltas_and_a_trailing_ground_flag() {
        let payload = [9, 0x10, 0x00, 0xff, 0x80, 0, 0, 64, 0xf0, 1];
        let id = ids::play::clientbound::MOVE_ENTITY_POS_ROT;
        let play::Clientbound::MoveEntityPosRot(play::MoveEntityPosRot(movement)) =
            decode_entity_move(id, &payload).unwrap()
        else {
            panic!("expected relative entity move");
        };
        assert_eq!(movement.id, 9);
        assert_eq!(
            movement.delta,
            Some(play::VecDelta::Linear([4096, -128, 0]))
        );
        assert_eq!(movement.rotation, Some((64, -16)));
        assert!(movement.on_ground);
        assert!(decode_entity_move(id, &payload[..payload.len() - 1]).is_err());
    }

    #[test]
    fn entity_sync_keeps_the_position_and_rotation_after_the_legacy_velocity() {
        let mut w = Writer::new();
        w.varint(91);
        Vec3d::write(&[-20.0, 68.0, 3.25], &mut w);
        Vec3d::write(&[0.5, -0.08, 1.0], &mut w);
        w.f32(90.0);
        w.f32(-15.0);
        w.bool(true);

        let packet = decode_entity_position_sync(&w.data).unwrap();
        assert_eq!(packet.id, 91);
        assert_eq!(packet.path, play::PositionPath::Linear([-20.0, 68.0, 3.25]));
        assert_eq!(
            (packet.yaw, packet.pitch, packet.on_ground),
            (90.0, -15.0, true)
        );
        assert!(decode_entity_position_sync(&w.data[..w.data.len() - 1]).is_err());
    }

    #[test]
    fn falling_block_spawn_remaps_both_entity_type_and_block_state() {
        let spawn = play::AddEntity {
            id: 1,
            uuid: 2,
            kind: 51,
            position: [0.0; 3],
            velocity: [0.0; 3],
            pitch: 0,
            yaw: 0,
            head_yaw: 0,
            data: 279,
        };
        let mapped = remap_entity_spawn(spawn).unwrap();
        assert_eq!((mapped.kind, mapped.data), (52, 294));
    }

    #[test]
    fn incremental_light_accepts_long_masks_and_rejects_short_layers() {
        // Chunk (-1, 2), with block light in layer zero and no sky light.
        let mut w = Writer::new();
        w.varint(-1);
        w.varint(2);
        w.long_array(&[]);
        w.long_array(&[1]);
        w.long_array(&[]);
        w.long_array(&[]);
        w.varint(0); // No sky arrays.
        w.varint(1); // One block array.
        let header = w.data.len();
        w.byte_array(&[0x1f; 2048]);
        let update = decode_light_update(&w.data).unwrap();
        assert_eq!((update.x, update.z), (-1, 2));
        assert_eq!(update.light.layers(1)[0].1.as_deref().unwrap()[0], 0x1f);

        w.data.truncate(header);
        w.byte_array(&[0x1f]);
        assert!(decode_light_update(&w.data).is_err());
    }

    #[test]
    fn block_palettes_translate_ids_from_vanilla_reports() {
        // Default minecraft:oak_leaves is 279 in 1.21.11 and 294 in 26.3.
        let blocks: Vec<i32> = (0..BLOCKS)
            .map(|i| if i % 2 == 0 { 279 } else { 1 })
            .collect();
        let original = Paletted::from_values(&blocks, Strategy::Blocks, 15);
        let remapped = remap_block_palette(original.clone()).unwrap();
        assert_eq!(remapped.data, original.data);
        assert_eq!([remapped.get(0), remapped.get(1)], [294, 1]);

        let blocks: Vec<i32> = (0..BLOCKS as i32).collect();
        let original = Paletted::from_values(&blocks, Strategy::Blocks, 15);
        assert!(original.palette.is_empty());
        let remapped = remap_block_palette(original).unwrap();
        assert_eq!(remapped.bits, 16);
        assert_eq!(
            [remapped.get(0), remapped.get(279), remapped.get(280)],
            [0, 294, 295]
        );

        assert!(
            remap_block_palette(Paletted::from_values(
                &[29671; BLOCKS],
                Strategy::Blocks,
                15
            ))
            .is_err()
        );
    }

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
