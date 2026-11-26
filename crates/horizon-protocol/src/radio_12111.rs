//! Wire formats for the experimental Minecraft Java 1.21.11 (protocol 774) session.

mod block_state_ranges;
mod entity_types;
mod item_types;
mod recipes;

pub use recipes::decode_recipe_book;

const LEGACY_MAP_COLOR: i32 = 43;

use crate::{
    Result,
    buf::{Bool, Byte, Codec, Ident, Int, List, Long, Opt, Pos, Reader, UByte, VarInt, Vec3d, Writer},
    chunk::{BLOCKS, ChunkData, Layers, LightData, Paletted, Strategy},
    entity_data::{self, Particle, ParticleData, Value, serializer},
    ids,
    item::{Component, Hashed, Stack, Template, component},
    packet,
    packets::{handshake, login, play},
};

/// Sound events and particle types in vanilla 1.21.11 registry order.
const SOUND_EVENTS: &str = include_str!("radio_12111/sound_events.txt");
const PARTICLE_TYPES: &str = include_str!("radio_12111/particle_types.txt");

pub fn sound_name(id: i32) -> Option<&'static str> {
    static NAMES: std::sync::LazyLock<Vec<&str>> =
        std::sync::LazyLock::new(|| SOUND_EVENTS.lines().collect());
    NAMES.get(usize::try_from(id).ok()?).copied()
}

pub fn particle_name(id: i32) -> Option<&'static str> {
    PARTICLE_TYPES.lines().nth(usize::try_from(id).ok()?)
}

fn explosion_particle(r: &mut Reader, target: BlockStateTarget) -> Result<Particle> {
    let id = r.varint()?;
    let name = particle_name(id).ok_or_else(|| crate::Error::new("unknown 1.21.11 particle"))?;
    // Reuse the shared codecs where the payload is unchanged. Return the
    // source particle ID so the client can resolve it by name.
    let codec = match name {
        "minecraft:block" | "minecraft:block_marker" | "minecraft:falling_dust"
        | "minecraft:dust_pillar" | "minecraft:block_crumble" => 1,
        "minecraft:entity_effect" | "minecraft:tinted_leaves" | "minecraft:flash" => 28,
        "minecraft:dust" => 21,
        "minecraft:dust_color_transition" => 22,
        "minecraft:effect" | "minecraft:instant_effect" => 23,
        "minecraft:dragon_breath" | "minecraft:sculk_charge" => 15,
        "minecraft:shriek" => 115,
        "minecraft:trail" => 59,
        "minecraft:vibration" => 58,
        "minecraft:item" => {
            let data = read_item_slot(r, target)?.map(ParticleData::Item);
            return Ok(Particle { id, data });
        }
        _ => 0, // Only the types above carry a payload in 1.21.11.
    };
    let mut particle = Particle::read_with_id(r, codec)?;
    particle.id = id;
    if let Some(ParticleData::State(state)) = &mut particle.data {
        *state = target.state(*state).ok_or_else(|| crate::Error::new("unknown 1.21.11 particle block state"))?;
    }
    Ok(particle)
}

/// 1.21.11 always plays the explosion sound. It has no trailing play flag.
pub fn decode_explosion(payload: &[u8], target: BlockStateTarget) -> Result<play::Explode> {
    let mut r = Reader::new(payload);
    let center = Vec3d::read(&mut r)?;
    let radius = r.f32()?;
    let block_count = r.i32()?;
    let knockback = Opt::<Vec3d>::read(&mut r)?;
    let particle = explosion_particle(&mut r, target)?;
    let sound = play::SoundHolder::read(&mut r)?;
    let count = r.count(r.remaining() / 10)?;
    let block_particles = (0..count)
        .map(|_| Ok(play::ExplosionParticle {
            particle: explosion_particle(&mut r, target)?,
            scaling: r.f32()?,
            speed: r.f32()?,
            weight: r.varint()?,
        }))
        .collect::<Result<_>>()?;
    if r.remaining() != 0 {
        return Err(crate::Error::new("trailing bytes in 1.21.11 explosion"));
    }
    Ok(play::Explode {
        center,
        radius,
        block_count,
        knockback,
        particle,
        sound,
        block_particles,
        play_sound: true,
    })
}

/// Names in the 1.21.11 attribute registry's wire order.
pub fn attribute_name(id: i32) -> Option<&'static str> {
    const NAMES: &[&str] = &[
        "minecraft:armor",
        "minecraft:armor_toughness",
        "minecraft:attack_damage",
        "minecraft:attack_knockback",
        "minecraft:attack_speed",
        "minecraft:block_break_speed",
        "minecraft:block_interaction_range",
        "minecraft:burning_time",
        "minecraft:camera_distance",
        "minecraft:explosion_knockback_resistance",
        "minecraft:entity_interaction_range",
        "minecraft:fall_damage_multiplier",
        "minecraft:flying_speed",
        "minecraft:follow_range",
        "minecraft:gravity",
        "minecraft:jump_strength",
        "minecraft:knockback_resistance",
        "minecraft:luck",
        "minecraft:max_absorption",
        "minecraft:max_health",
        "minecraft:mining_efficiency",
        "minecraft:movement_efficiency",
        "minecraft:movement_speed",
        "minecraft:oxygen_bonus",
        "minecraft:safe_fall_distance",
        "minecraft:scale",
        "minecraft:sneaking_speed",
        "minecraft:spawn_reinforcements",
        "minecraft:step_height",
        "minecraft:submerged_mining_speed",
        "minecraft:sweeping_damage_ratio",
        "minecraft:tempt_range",
        "minecraft:water_movement_efficiency",
        "minecraft:waypoint_transmit_range",
        "minecraft:waypoint_receive_range",
    ];
    NAMES.get(usize::try_from(id).ok()?).copied()
}

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
pub fn remap_entity_spawn(spawn: play::AddEntity) -> Result<play::AddEntity> {
    remap_entity_spawn_with_blocks(spawn, BlockStateTarget::Engine26_3)
}

pub fn remap_entity_spawn_with_blocks(
    mut spawn: play::AddEntity,
    target: BlockStateTarget,
) -> Result<play::AddEntity> {
    if spawn.kind == entity_types::FALLING_BLOCK {
        spawn.data = target
            .state(spawn.data)
            .ok_or_else(|| crate::Error::new("unknown 1.21.11 falling block state"))?;
    }
    spawn.kind = match target {
        BlockStateTarget::Native12111 => native_id(spawn.kind, entity_types::ENTITY_TYPES.len()),
        BlockStateTarget::Engine26_3 => usize::try_from(spawn.kind)
            .ok()
            .and_then(|id| entity_types::ENTITY_TYPES.get(id).copied())
            .map(i32::from),
    }
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

fn native_id(id: i32, len: usize) -> Option<i32> {
    usize::try_from(id).ok().filter(|&i| i < len).map(|_| id)
}

impl BlockStateTarget {
    pub fn block_entity(self, id: i32) -> Option<i32> {
        match self {
            Self::Native12111 => native_id(id, entity_types::BLOCK_ENTITY_TYPES.len()),
            Self::Engine26_3 => block_entity_type_to_engine(id),
        }
    }
}

/// Decode a 1.21.11 Level Chunk With Light payload after removing its packet ID.
pub fn decode_level_chunk(payload: &[u8]) -> Result<play::LevelChunkWithLight> {
    decode_level_chunk_for(payload, BlockStateTarget::Engine26_3)
}

pub fn decode_level_chunk_for(
    payload: &[u8],
    target: BlockStateTarget,
) -> Result<play::LevelChunkWithLight> {
    let mut r = Reader::new(payload);
    let x = r.i32()?;
    let z = r.i32()?;
    let mut chunk = ChunkData::read(&mut r)?;
    chunk.block_entities.retain_mut(|entity| {
        if let Some(kind) = target.block_entity(entity.kind) {
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

/// Registry used by the loaded block models and collision tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockStateTarget {
    Native12111,
    Engine26_3,
}

impl BlockStateTarget {
    /// A representative state identifies the block in the client's loaded facts.
    pub fn block_event_state(self, block: i32) -> Option<i32> {
        let source = *block_state_ranges::BLOCK_EVENT_STATES.get(usize::try_from(block).ok()?)?;
        self.state(i32::from(source))
    }

    pub fn state(self, source: i32) -> Option<i32> {
        match self {
            Self::Native12111 => (0..block_state_ranges::SOURCE_LEN as i32)
                .contains(&source)
                .then_some(source),
            Self::Engine26_3 => block_state_to_engine(source),
        }
    }

    pub fn palette(self, blocks: Paletted) -> Result<Paletted> {
        if self == Self::Engine26_3 {
            return remap_block_palette(blocks);
        }
        // The native registry uses the packet's exact packing. Still validate
        // the server-supplied IDs before handing the storage to the renderer.
        if blocks.palette.is_empty() {
            if blocks.bits != block_state_ranges::SOURCE_BITS
                || blocks.data.len() != crate::chunk::storage_len(blocks.bits, BLOCKS)
            {
                return Err(crate::Error::new("invalid 1.21.11 direct block palette"));
            }
            if (0..BLOCKS).any(|index| self.state(blocks.get(index)).is_none()) {
                return Err(crate::Error::new("unknown 1.21.11 direct block state"));
            }
        } else if blocks.palette.iter().any(|&id| self.state(id).is_none()) {
            return Err(crate::Error::new("unknown 1.21.11 palette state"));
        }
        Ok(blocks)
    }
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
        // Both registries fit four entries per long. Translate one word at a
        // time, ignoring the four padding bits in each incoming 15-bit word.
        const SRC: u8 = block_state_ranges::SOURCE_BITS;
        const DST: u8 = block_state_ranges::TARGET_BITS;
        const PER_WORD: u8 = 64 / SRC;
        const { assert!(PER_WORD == 64 / DST && BLOCKS % PER_WORD as usize == 0) };
        for word in &mut blocks.data {
            let source = *word as u64;
            let mut target = 0u64;
            for slot in 0..PER_WORD {
                let id = ((source >> (slot * SRC)) & ((1 << SRC) - 1)) as i32;
                target |= (remap(id)? as u64) << (slot * DST);
            }
            *word = target as i64;
        }
        blocks.bits = DST;
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

/// 1.21.11 sends the action and, for an armor stand, the hit offset as floats.
pub fn encode_interact(interact: &play::Interact, at_location: bool) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.varint(ids::play::serverbound::INTERACT);
    writer.varint(interact.id);
    writer.varint(if at_location { 2 } else { 0 });
    if at_location {
        for coordinate in interact.location {
            writer.f32(coordinate as f32);
        }
    }
    writer.varint(interact.hand);
    writer.bool(interact.secondary);
    writer.data
}

/// 1.21.11 sends a boolean before the four lines. The engine puts the side last.
pub fn encode_sign_update(update: &play::SignUpdate) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.varint(ids::play::serverbound::SIGN_UPDATE);
    Pos::write(&update.pos, &mut writer);
    writer.bool(update.front);
    play::SignLines::write(&update.lines, &mut writer);
    writer.data
}

/// 1.21.11 puts arm swings in Animate and leaves action 1 unused.
pub fn decode_animation(payload: &[u8]) -> Result<Option<play::Clientbound>> {
    let animation = play::Animate::read(&mut Reader::new(payload))?;
    Ok(match animation.action {
        action @ (0 | 3) => Some(play::Clientbound::SwingAnimation(play::SwingAnimation {
            id: animation.id,
            hand: i32::from(action == 3),
            kind: 1, // whack
            duration: 6,
        })),
        2 | 4 | 5 => Some(play::Clientbound::Animate(play::Animate {
            id: animation.id,
            action: match animation.action {
                2 => 0, // leave bed
                4 => 1, // critical hit
                _ => 2, // enchanted hit
            },
        })),
        _ => None,
    })
}

/// Components inside Rust stacks use the protocol crate's canonical ordinals.
/// Keep the loaded registry's names paired with those ordinals for NBT and click hashes.
pub fn canonicalize_component_registry(builtin: &mut crate::registry::Builtin) {
    let Some(components) = builtin.registries.get_mut("minecraft:data_component_type") else {
        return;
    };
    let names = std::mem::take(&mut components.names);
    components.ids.clear();
    for (name, &canonical) in names.into_iter().zip(item_types::COMPONENTS.iter()) {
        let Ok(index) = usize::try_from(canonical) else {
            continue;
        };
        if components.names.len() <= index {
            components.names.resize(index + 1, String::new());
        }
        components.ids.insert(name.clone(), i32::from(canonical));
        components.names[index] = name;
    }
}

fn item_registry_id(id: i32, lookup: &[i16]) -> Result<i32> {
    usize::try_from(id)
        .ok()
        .and_then(|id| lookup.get(id).copied())
        .filter(|&id| id >= 0)
        .map(i32::from)
        .ok_or_else(|| crate::Error::new(format!("unmapped 1.21.11 item registry ID {id}")))
}

fn read_item_slot(reader: &mut Reader, target: BlockStateTarget) -> Result<Option<Stack>> {
    read_nested_item_slot(reader, target, 0)
}

fn read_nested_item_slot(
    reader: &mut Reader,
    target: BlockStateTarget,
    depth: usize,
) -> Result<Option<Stack>> {
    if depth > 16 {
        return Err(crate::Error::new("1.21.11 item nesting limit exceeded"));
    }
    let count = reader.varint()?;
    if count <= 0 {
        return Ok(None);
    }
    let id = reader.varint()?;
    let item = match target {
        BlockStateTarget::Engine26_3 => item_registry_id(id, &item_types::ITEMS)?,
        BlockStateTarget::Native12111 => native_id(id, item_types::ITEMS.len())
            .ok_or_else(|| crate::Error::new("unknown 1.21.11 item"))?,
    };
    let mut stack = Stack::new(item, count);
    let added = reader.count(104)?;
    let removed = reader.count(104)?;
    for _ in 0..added {
        let source_kind = reader.varint()?;
        // The engine has no map_color component. Read past its RGB tint
        // so a tinted map keeps its ID and the rest of the inventory decodes.
        if source_kind == LEGACY_MAP_COLOR {
            reader.i32()?;
            continue;
        }
        let kind = item_registry_id(source_kind, &item_types::COMPONENTS)?;
        let mut value = match kind {
            component::TRIM => {
                let material = reader.varint()?;
                if material == 0 {
                    reader.string(32767)?;
                    let overrides = reader.count(reader.remaining() / 2)?;
                    for _ in 0..overrides {
                        reader.string(32767)?;
                        reader.string(32767)?;
                    }
                    reader.nbt()?;
                }
                let pattern = reader.varint()?;
                if pattern == 0 {
                    reader.string(32767)?;
                    reader.nbt()?;
                    reader.bool()?;
                }
                if material < 0 || pattern < 0 {
                    return Err(crate::Error::new("invalid 1.21.11 armor trim holder"));
                }
                // Inline trims have no synchronized registry ID to hand to the renderer.
                if material == 0 || pattern == 0 {
                    continue;
                }
                Component::Trim {
                    material: material - 1,
                    pattern: pattern - 1,
                }
            }
            component::CONTAINER | component::BUNDLE_CONTENTS | component::CHARGED_PROJECTILES => {
                let count = reader.count(reader.remaining().min(1024))?;
                let items = (0..count)
                    .map(|_| read_nested_item_slot(reader, target, depth + 1))
                    .collect::<Result<Vec<_>>>()?;
                if kind == component::CONTAINER {
                    let mut w = Writer::new();
                    w.varint(count as i32);
                    for item in items {
                        <Opt<Template>>::write(&item, &mut w);
                    }
                    Component::Raw(w.data)
                } else {
                    Component::Stacks(items.into_iter().flatten().collect())
                }
            }
            component::CUSTOM_DATA
            | component::MAX_STACK_SIZE
            | component::MAX_DAMAGE
            | component::DAMAGE
            | component::UNBREAKABLE
            | component::USE_EFFECTS
            | component::MINIMUM_ATTACK_CHARGE
            | component::CREATIVE_SLOT_LOCK
            | component::ATTACK_RANGE
            | component::POTION_DURATION_SCALE
            | component::DEBUG_STICK_STATE
            | component::ENTITY_DATA
            | component::BUCKET_ENTITY_DATA
            | component::BLOCK_ENTITY_DATA
            | component::RECIPES
            | component::LODESTONE_TRACKER
            | component::NOTE_BLOCK_SOUND
            | component::LOCK
            | component::CONTAINER_LOOT
            | component::CUSTOM_NAME
            | component::ITEM_NAME
            | component::ITEM_MODEL
            | component::LORE
            | component::RARITY
            | component::ENCHANTMENTS
            | component::STORED_ENCHANTMENTS
            | component::REPAIR_COST
            | component::CUSTOM_MODEL_DATA
            | component::DYED_COLOR
            | component::POTION_CONTENTS
            | component::SUSPICIOUS_STEW_EFFECTS
            | component::INTANGIBLE_PROJECTILE
            | component::MAP_ID
            | component::MAP_DECORATIONS
            | component::MAP_POST_PROCESSING
            | component::OMINOUS_BOTTLE_AMPLIFIER
            | component::TOOLTIP_DISPLAY
            | component::TOOLTIP_STYLE
            | component::ATTRIBUTE_MODIFIERS
            | component::FOOD
            | component::USE_COOLDOWN
            | component::TOOL
            | component::WEAPON
            | component::ENCHANTABLE
            | component::EQUIPPABLE
            | component::REPAIRABLE
            | component::GLIDER
            | component::BLOCK_STATE
            | component::BASE_COLOR
            | component::BANNER_PATTERNS
            | component::WRITABLE_BOOK_CONTENT
            | component::WRITTEN_BOOK_CONTENT
            | component::PROFILE
            | component::FIREWORKS
            | component::FIREWORK_EXPLOSION
            | component::ENCHANTMENT_GLINT_OVERRIDE => Component::read(kind, reader)?,
            // 1.21.11 does not include per-component lengths here. Refuse the
            // packet if the component's wire format has not been checked.
            _ => {
                return Err(crate::Error::new(format!(
                    "unsupported 1.21.11 item component {kind}"
                )));
            }
        };
        if let Component::TooltipDisplay { hidden, .. } = &mut value {
            hidden.retain_mut(|id| match item_registry_id(*id, &item_types::COMPONENTS) {
                Ok(mapped) => {
                    *id = mapped;
                    true
                }
                Err(_) => false,
            });
        }
        stack.added.push((kind, value));
    }
    for _ in 0..removed {
        let source_kind = reader.varint()?;
        if source_kind != LEGACY_MAP_COLOR {
            stack
                .removed
                .push(item_registry_id(source_kind, &item_types::COMPONENTS)?);
        }
    }
    Ok(Some(stack))
}

/// Read the server's Brigadier tree using 1.21.11 argument IDs.
pub fn decode_commands(payload: &[u8]) -> Result<play::Commands> {
    let mut r = Reader::new(payload);
    let commands = play::Commands::read_with_parser_ids(&mut r, |id| {
        match id {
            0..=54 => Ok(id),
            55 => Ok(58), // dialog
            56 => Ok(61), // uuid
            _ => Err(crate::Error::new(format!("unknown 1.21.11 command parser {id}"))),
        }
    })?;
    if r.remaining() != 0 {
        return Err(crate::Error::new("trailing bytes in 1.21.11 command tree"));
    }
    Ok(commands)
}

/// Translate the 1.21.11 team parameter layout into the shared HUD model.
pub fn decode_team(payload: &[u8]) -> Result<play::SetPlayerTeam> {
    let mut r = Reader::new(payload);
    let name = r.string(32767)?;
    let method = r.i8()?;
    let parameters = if matches!(method, 0 | 2) {
        let display_name = r.nbt()?;
        let options = r.i8()?;
        let name_tag_visibility = r.varint()?;
        let collision_rule = r.varint()?;
        let formatting = r.varint()?;
        Some(play::TeamParameters {
            display_name,
            prefix: r.nbt()?,
            suffix: r.nbt()?,
            name_tag_visibility,
            collision_rule,
            color: (0..16).contains(&formatting).then_some(formatting),
            options,
        })
    } else {
        None
    };
    let players = if matches!(method, 0 | 3 | 4) {
        <List<crate::buf::Str>>::read(&mut r)?
    } else {
        Vec::new()
    };
    if !matches!(method, 0..=4) || r.remaining() != 0 {
        return Err(crate::Error::new("invalid 1.21.11 team update"));
    }
    Ok(play::SetPlayerTeam {
        name,
        method,
        parameters,
        players,
    })
}

/// Read shared metadata fields, including the invisibility and name flags
/// used by armor stand holograms. Unframed serializers with version-specific
/// layouts stop the scan, preserving the fields already read.
pub fn decode_entity_data(payload: &[u8], target: BlockStateTarget) -> Result<play::SetEntityData> {
    let mut r = Reader::new(payload);
    let id = r.varint()?;
    let mut values = Vec::new();
    loop {
        let index = r.u8()?;
        if index == 0xff {
            break;
        }
        let kind = r.varint()?;
        let value = match kind {
            serializer::ITEM_STACK => match read_item_slot(&mut r, target) {
                Ok(item) => Value::Item(item),
                Err(_) => break,
            },
            serializer::PARTICLES => {
                // An empty list clears potion particles and has no
                // version-specific particle IDs or payloads to translate.
                if r.varint()? != 0 {
                    break;
                }
                Value::Particles(Vec::new())
            }
            serializer::BYTE..=serializer::DIRECTION
            | serializer::OPTIONAL_UNSIGNED_INT
            | serializer::POSE => {
                let Some(value) = entity_data::read_value(kind, &mut r)? else {
                    break;
                };
                value
            }
            _ => break,
        };
        values.push((index, kind, value));
    }
    Ok(play::SetEntityData { id, values })
}

pub fn decode_inventory(id: i32, payload: &[u8]) -> Result<play::Clientbound> {
    decode_inventory_for(id, payload, BlockStateTarget::Engine26_3)
}

pub fn decode_inventory_for(
    id: i32,
    payload: &[u8],
    target: BlockStateTarget,
) -> Result<play::Clientbound> {
    use ids::play::clientbound as canonical;
    let mut reader = Reader::new(payload);
    let packet = match id {
        canonical::CONTAINER_SET_CONTENT => {
            let container = reader.varint()?;
            let state = reader.varint()?;
            let count = reader.count(reader.remaining())?;
            let items = (0..count)
                .map(|slot| {
                    read_item_slot(&mut reader, target).map_err(|err| {
                        crate::Error::new(format!("container {container}, slot {slot}: {err}"))
                    })
                })
                .collect::<Result<_>>()?;
            play::Clientbound::ContainerSetContent(play::ContainerSetContent {
                container,
                state,
                items,
                carried: read_item_slot(&mut reader, target)?,
            })
        }
        canonical::CONTAINER_SET_SLOT => {
            play::Clientbound::ContainerSetSlot(play::ContainerSetSlot {
                container: reader.varint()?,
                state: reader.varint()?,
                slot: reader.i16()?,
                item: read_item_slot(&mut reader, target)?,
            })
        }
        canonical::SET_PLAYER_INVENTORY => {
            play::Clientbound::SetPlayerInventory(play::SetPlayerInventory {
                slot: reader.varint()?,
                item: read_item_slot(&mut reader, target)?,
            })
        }
        canonical::SET_CURSOR_ITEM => play::Clientbound::SetCursorItem(play::SetCursorItem {
            item: read_item_slot(&mut reader, target)?,
        }),
        canonical::SET_EQUIPMENT => {
            let id = reader.varint()?;
            let mut slots = Vec::new();
            loop {
                let slot = reader.u8()?;
                slots.push((slot & 0x7f, read_item_slot(&mut reader, target)?));
                if slot & 0x80 == 0 {
                    break;
                }
            }
            play::Clientbound::SetEquipment(play::SetEquipment { id, slots })
        }
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

fn remap_slot_hash(slot: &mut Option<Hashed>, target: BlockStateTarget) -> bool {
    let Some(stack) = slot else {
        return true;
    };
    let item = match target {
        BlockStateTarget::Engine26_3 => legacy_registry_id(stack.item, &item_types::ITEMS),
        BlockStateTarget::Native12111 => native_id(stack.item, item_types::ITEMS.len()),
    };
    let Some(item) = item else {
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
pub fn remap_container_click(click: play::ContainerClick) -> play::ContainerClick {
    remap_container_click_for(click, BlockStateTarget::Engine26_3)
}

pub fn remap_container_click_for(
    mut click: play::ContainerClick,
    target: BlockStateTarget,
) -> play::ContainerClick {
    click
        .changed
        .retain_mut(|(_, prediction)| remap_slot_hash(prediction, target));
    if !remap_slot_hash(&mut click.carried, target) {
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
    fn interaction_and_sign_actions_match_the_12111_wire_layout() {
        let interact = play::Interact {
            id: 5,
            hand: 1,
            location: [0.25, 1.0, -0.5],
            secondary: true,
        };
        let packet = encode_interact(&interact, false);
        assert_eq!(packet, [26, 5, 0, 1, 1]);
        let packet = encode_interact(&interact, true);
        assert_eq!(packet, [26, 5, 2, 62, 128, 0, 0, 63, 128, 0, 0, 191, 0, 0, 0, 1, 1]);

        let update = play::SignUpdate {
            pos: [0; 3],
            lines: ["a".into(), "b".into(), "".into(), "d".into()],
            front: true,
        };
        let packet = encode_sign_update(&update);
        assert_eq!(&packet[..10], &[62, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(&packet[10..], b"\x01a\x01b\x00\x01d");
    }

    #[test]
    fn map_tint_and_intangible_ammo_keep_the_rest_of_the_stack() {
        let mut w = Writer::new();
        // The unmapped tint precedes the map id and decorations.
        w.bytes(&[1, 1, 3, 0, 43]);
        w.i32(0x123456);
        w.bytes(&[44, 19, 45, 10, 0]);
        let mut reader = Reader::new(&w.data);
        let stack = read_item_slot(&mut reader, BlockStateTarget::Native12111)
            .unwrap()
            .unwrap();
        assert_eq!(reader.remaining(), 0);
        assert_eq!(stack.added[0], (component::MAP_ID, Component::Int(19)));

        // A charged crossbow contains an arrow with the NBT-backed unit marker.
        let data = [1, 1, 1, 0, 47, 1, 1, 1, 1, 0, 22, 10, 0];
        let mut reader = Reader::new(&data);
        let stack = read_item_slot(&mut reader, BlockStateTarget::Native12111)
            .unwrap()
            .unwrap();
        assert_eq!(reader.remaining(), 0);
        assert!(matches!(
            &stack.added[..],
            [(component::CHARGED_PROJECTILES, Component::Stacks(ammo))] if ammo.len() == 1
        ));
    }

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
        let native = remap_container_click_for(click.clone(), BlockStateTarget::Native12111);
        assert_eq!(native.changed[1].1.as_ref().unwrap().item, 128);
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
    fn equipment_translates_held_items_and_preserves_empty_slots() {
        // Entity 42, main hand with a damaged golden pickaxe, empty offhand.
        let payload = [42, 0x80, 1, 0xa0, 7, 1, 0, 3, 11, 1, 0];
        let id = ids::play::clientbound::SET_EQUIPMENT;
        for (target, expected_item) in [
            (BlockStateTarget::Engine26_3, 1042),
            (BlockStateTarget::Native12111, 928),
        ] {
            let play::Clientbound::SetEquipment(packet) =
                decode_inventory_for(id, &payload, target).unwrap()
            else {
                panic!("expected entity equipment update");
            };
            assert_eq!(packet.id, 42);
            assert_eq!(packet.slots.len(), 2);
            let (slot, stack) = &packet.slots[0];
            let stack = stack.as_ref().unwrap();
            assert_eq!((*slot, stack.item, stack.damage()), (0, expected_item, 11));
            assert_eq!(packet.slots[1], (1, None));
        }
        assert!(decode_inventory(id, &payload[..9]).is_err());
        assert!(decode_inventory(id, &[42, 0, 0, 0]).is_err());
    }

    #[test]
    fn equipment_preserves_dye_and_resource_pack_model_data() {
        let mut w = Writer::new();
        w.varint(42); // Entity ID.
        w.u8(0); // Main hand.
        w.varint(1); // Item count.
        w.varint(928); // Golden pickaxe with custom components.
        w.varint(2); // Two added components.
        w.varint(0); // No removed components.
        w.varint(42); // 1.21.11 dyed_color component ID.
        w.i32(0x33_66_cc);
        w.varint(17); // 1.21.11 custom_model_data component ID.
        w.varint(0); // Floats.
        w.varint(0); // Flags.
        w.varint(0); // Strings.
        w.varint(1); // Colors.
        w.i32(0xff_99_cc);

        let play::Clientbound::SetEquipment(packet) =
            decode_inventory(ids::play::clientbound::SET_EQUIPMENT, &w.data).unwrap()
        else {
            panic!("expected entity equipment update");
        };
        let stack = packet.slots[0].1.as_ref().unwrap();
        assert_eq!(
            stack.component(component::DYED_COLOR),
            Some(&Component::Int(0x33_66_cc))
        );
        assert!(matches!(
            stack.component(component::CUSTOM_MODEL_DATA),
            Some(Component::CustomModelData { colors, .. }) if colors.as_slice() == [0xff_99_cc]
        ));
    }

    #[test]
    fn inventory_snapshot_reads_trimmed_gear_shield_colors_and_head_profiles() {
        let mut w = Writer::new();
        for value in [0, 1, 4, 1, 928, 1, 0, 54, 2, 3, 1, 928, 2, 0, 71, 14, 70, 1, 1, 14] {
            w.varint(value);
        }
        for value in [1, 928, 1, 0, 68, 0] {
            w.varint(value);
        }
        w.bool(true);
        w.string("Player");
        w.bool(false);
        w.varint(0);
        for _ in 0..4 {
            w.bool(false);
        }
        // A final ordinary stack and an empty cursor.
        for value in [8, 1, 0, 0, 0] {
            w.varint(value);
        }

        let play::Clientbound::ContainerSetContent(packet) =
            decode_inventory(ids::play::clientbound::CONTAINER_SET_CONTENT, &w.data).unwrap()
        else {
            panic!("expected inventory snapshot");
        };
        let component = |slot: usize, kind| packet.items[slot].as_ref().unwrap().component(kind);
        assert_eq!(
            component(0, component::TRIM),
            Some(&Component::Trim { material: 1, pattern: 2 })
        );
        assert!(component(1, component::BASE_COLOR).is_some());
        assert!(matches!(
            component(1, component::BANNER_PATTERNS),
            Some(Component::BannerPatterns(layers)) if layers.len() == 1
        ));
        assert!(matches!(component(2, component::PROFILE), Some(Component::Profile(_))));
        assert_eq!(packet.items[3].as_ref().unwrap().count, 8);
        assert!(packet.carried.is_none());
    }

    #[test]
    fn container_snapshot_reads_past_written_and_writable_books() {
        use horizon_world::nbt::Tag;

        let mut w = Writer::new();
        // Container 1, state 2, three slots. The first slot is a written book.
        for value in [1, 2, 3, 1, 1221, 1, 0, 53] {
            w.varint(value);
        }
        w.string("Notes");
        w.bool(false); // No filtered title.
        w.string("Author");
        w.varint(0); // Original generation.
        w.varint(1); // One page, with both raw and filtered text.
        w.nbt(&Tag::String("Raw page".into()));
        w.bool(true);
        w.nbt(&Tag::String("Filtered page".into()));
        w.bool(true); // Resolved.

        for value in [1, 1220, 1, 0, 52, 1] {
            w.varint(value);
        }
        w.string("Draft page");
        w.bool(false);

        // Eight stone in the final slot and an empty cursor.
        for value in [8, 1, 0, 0, 0] {
            w.varint(value);
        }
        let play::Clientbound::ContainerSetContent(packet) = decode_inventory_for(
            ids::play::clientbound::CONTAINER_SET_CONTENT,
            &w.data,
            BlockStateTarget::Native12111,
        )
        .unwrap() else {
            panic!("expected container snapshot");
        };
        let written = packet.items[0].as_ref().unwrap();
        assert_eq!(written.item, 1221);
        assert!(matches!(
            written.component(component::WRITTEN_BOOK_CONTENT),
            Some(Component::WrittenBook(book)) if book.pages.len() == 1
        ));
        let writable = packet.items[1].as_ref().unwrap();
        assert_eq!(writable.item, 1220);
        assert!(matches!(
            writable.component(component::WRITABLE_BOOK_CONTENT),
            Some(Component::WritableBook { pages, .. }) if pages.len() == 1
        ));
        assert_eq!(packet.items[2].as_ref().unwrap().count, 8);
        assert!(packet.carried.is_none());
    }

    #[test]
    fn inventory_snapshot_keeps_entity_data_and_slots_after_a_lodestone_compass() {
        let mut w = Writer::new();
        w.varint(0); // Player inventory.
        w.varint(5); // State ID.
        w.varint(4);
        // An axolotl bucket and a chest with block entity data.
        for (item_id, source_kind) in [(1023, 57), (331, 58)] {
            for field in [1, item_id, 1, 0, source_kind] {
                w.varint(field);
            }
            if source_kind == 58 {
                w.varint(2); // Block entity type.
            }
            w.bytes(&[10, 0]); // Empty unnamed compound.
        }
        for field in [1, 1034, 1, 0, 65] {
            w.varint(field);
        }
        w.bool(true);
        w.string("minecraft:overworld");
        w.i64(0);
        w.bool(true);
        // Last slot and carried stack.
        for field in [32, 1, 0, 0, 2, 1, 0, 0] {
            w.varint(field);
        }

        let play::Clientbound::ContainerSetContent(packet) = decode_inventory_for(
            ids::play::clientbound::CONTAINER_SET_CONTENT,
            &w.data,
            BlockStateTarget::Native12111,
        )
        .unwrap() else {
            panic!("expected inventory snapshot");
        };
        let item = |slot: usize| packet.items[slot].as_ref().unwrap();
        assert_eq!(
            item(0).component(component::BUCKET_ENTITY_DATA),
            Some(&Component::Raw(vec![10, 0]))
        );
        assert_eq!(
            item(1).component(component::BLOCK_ENTITY_DATA),
            Some(&Component::Raw(vec![2, 10, 0]))
        );
        assert!(matches!(item(2).component(component::LODESTONE_TRACKER),
            Some(Component::LodestoneTracker { target: Some((dim, _)), tracked: true })
            if dim == "minecraft:overworld"));
        assert_eq!(item(3).count, 32);
        assert_eq!(packet.carried.unwrap().count, 2);
    }

    #[test]
    fn inventory_snapshot_keeps_slots_after_a_filled_shulker_box() {
        let mut w = Writer::new();
        for value in [0, 1, 2, 1, 581, 1, 0, 73, 2, 0, 64, 1, 0, 0, 8, 1, 0, 0, 0] {
            w.varint(value);
        }
        let play::Clientbound::ContainerSetContent(packet) = decode_inventory_for(
            ids::play::clientbound::CONTAINER_SET_CONTENT,
            &w.data,
            BlockStateTarget::Native12111,
        )
        .unwrap() else {
            panic!("expected inventory snapshot");
        };
        let box_stack = packet.items[0].as_ref().unwrap();
        assert_eq!(box_stack.item, 581);
        let Some(Component::Raw(bytes)) = box_stack.component(component::CONTAINER) else {
            panic!("expected shulker contents");
        };
        let mut r = Reader::new(bytes);
        assert_eq!(r.varint().unwrap(), 2);
        assert!(<Opt<Template>>::read(&mut r).unwrap().is_none());
        let stone = <Opt<Template>>::read(&mut r).unwrap().unwrap();
        assert_eq!((stone.item, stone.count), (1, 64));
        assert_eq!(packet.items[1].as_ref().unwrap().count, 8);
        assert!(packet.carried.is_none());
    }

    #[test]
    fn command_tree_remaps_dialog_and_uuid_parsers() {
        let mut w = Writer::new();
        w.varint(3);
        w.u8(0); // Root with two children.
        w.varint(2);
        w.varint(1);
        w.varint(2);
        for (name, parser) in [("dialog", 55), ("uuid", 56)] {
            w.u8(6); // Executable argument node.
            w.varint(0);
            w.string(name);
            w.varint(parser);
        }
        w.varint(0);
        let tree = decode_commands(&w.data).unwrap();
        for (node, expected) in tree.nodes[1..].iter().zip(["minecraft:dialog", "minecraft:uuid"]) {
            let (parser, _) = node.parser.as_ref().unwrap();
            assert_eq!(play::ARGUMENT_TYPES[*parser as usize], expected);
        }
    }

    #[test]
    fn team_updates_read_legacy_formatting_before_prefix_and_suffix() {
        use horizon_world::nbt::Tag;

        let mut w = Writer::new();
        w.string("sidebar");
        w.i8(0);
        w.nbt(&Tag::String("Sidebar".into()));
        w.i8(3);
        w.varint(1);
        w.varint(2);
        w.varint(10);
        w.nbt(&Tag::String("Prefix ".into()));
        w.nbt(&Tag::String(" Suffix".into()));
        w.varint(1);
        w.string("entry");

        let team = decode_team(&w.data).unwrap();
        let parameters = team.parameters.unwrap();
        assert_eq!(team.players, vec!["entry".to_owned()]);
        assert_eq!(parameters.prefix, Tag::String("Prefix ".into()));
        assert_eq!(parameters.suffix, Tag::String(" Suffix".into()));
        assert_eq!(parameters.color, Some(10));
        assert_eq!(parameters.options, 3);
    }

    #[test]
    fn metadata_reads_invisibility_and_marker_flags_for_holograms() {
        let packet = decode_entity_data(
            &[42, 0, 0, 0x20, 3, 8, 1, 15, 0, 0x10, 0xff],
            BlockStateTarget::Engine26_3,
        )
        .unwrap();
        assert_eq!(packet.id, 42);
        assert_eq!(
            packet.values,
            vec![
                (0, serializer::BYTE, Value::Byte(0x20)),
                (3, serializer::BOOLEAN, Value::Bool(true)),
                (15, serializer::BYTE, Value::Byte(0x10)),
            ],
        );
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
        assert_eq!(BlockStateTarget::Native12111.block_entity(25), Some(25));
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
        let native =
            remap_entity_spawn_with_blocks(spawn.clone(), BlockStateTarget::Native12111).unwrap();
        assert_eq!((native.kind, native.data), (51, 279));
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
    fn direct_block_words_ignore_padding_and_translate_all_four_slots() {
        let mut blocks = Paletted {
            bits: 15,
            palette: Vec::new(),
            data: vec![
                (0xf000_0000_0000_0000u64 | 1 | (279 << 15) | (280 << 30) | (27 << 45))
                    as i64;
                BLOCKS / 4
            ],
        };
        let native = BlockStateTarget::Native12111
            .palette(blocks.clone())
            .unwrap();
        assert_eq!(native.bits, 15);
        assert_eq!(native.data, blocks.data);
        let original = blocks.clone();
        let mapped = remap_block_palette(original).unwrap();
        for index in 0..BLOCKS {
            assert_eq!(mapped.get(index), [1, 294, 295, 28][index % 4]);
        }
        blocks.data[BLOCKS / 4 - 1] |= (0x7fff_i64) << 45;
        assert!(
            BlockStateTarget::Native12111
                .palette(blocks.clone())
                .is_err()
        );
        assert!(remap_block_palette(blocks).is_err());
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
