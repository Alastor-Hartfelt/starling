//! Headless 1.21.11 join using the multiplayer session shared with the game.
//! Run: cargo run -p horizon-client-lib --example radio_12111_probe -- 127.0.0.1:25565
use horizon_client_lib::net::{Event, Session};
use horizon_protocol::{
    crypto::offline_uuid,
    packets::{Empty, configuration, play},
    radio_12111::{decode_chunk_sections, remap_block_palette},
    registry::{Dynamic, ceil_log2},
    text::Language,
};
use std::{
    error::Error,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

type ProbeResult<T> = Result<T, Box<dyn Error>>;

struct Level {
    name: String,
    sections: usize,
    saw_chunk: bool,
    teleported: bool,
    ready: bool,
}

impl Level {
    fn entered(spawn: play::SpawnInfo, registries: &Dynamic) -> ProbeResult<Self> {
        if registries
            .name("minecraft:dimension_type", spawn.dimension_type)
            .is_none()
        {
            return Err("spawn names an unsynchronized dimension type".into());
        }
        Ok(Self {
            name: spawn.dimension,
            sections: registries.dimension(spawn.dimension_type).sections(),
            saw_chunk: false,
            teleported: false,
            ready: false,
        })
    }
}

fn run(session: &Session, require_nether: bool, peer: Option<u128>) -> ProbeResult<()> {
    use play::{Clientbound as C, Serverbound as S};

    let deadline = Instant::now()
        + Duration::from_secs(if require_nether || peer.is_some() {
            75
        } else {
            55
        });
    let mut peer_listed = false;
    let mut peer_removed = false;
    let mut registries = None;
    let mut level: Option<Level> = None;
    let mut position = [0.0; 3];
    let (mut yaw, mut pitch) = (0.0, 0.0);
    let mut next_tick = Instant::now();
    let mut nether_ready = false;
    let (mut chunks, mut batches, mut keepalives) = (0, 0, 0);
    while Instant::now() < deadline {
        for event in session.poll() {
            let packet = match event {
                Event::Joined {
                    registries: r,
                    uuid,
                    name,
                } => {
                    if uuid != offline_uuid(&session.name) || name != session.name {
                        return Err("server returned an unexpected offline profile".into());
                    }
                    println!("login profile accepted");
                    println!("configuration complete");
                    registries = Some(r);
                    continue;
                }
                Event::Reconfigured(r) => {
                    registries = Some(r);
                    level = None;
                    continue;
                }
                Event::Packet(packet) => *packet,
                Event::Disconnected { reason, .. } | Event::DecodeError(reason) => {
                    return Err(reason.into());
                }
                _ => continue,
            };
            let registries = registries
                .as_ref()
                .ok_or("received play before configuration")?;
            match packet {
                C::Login(p) => {
                    println!(
                        "entered play as entity {} in {}",
                        p.player_id, p.spawn.dimension
                    );
                    level = Some(Level::entered(p.spawn, registries)?);
                }
                C::Respawn(p) => level = Some(Level::entered(p.spawn, registries)?),
                C::PlayerPosition(p) => {
                    use play::relative;
                    for (i, flag) in [relative::X, relative::Y, relative::Z]
                        .into_iter()
                        .enumerate()
                    {
                        position[i] = p.position[i]
                            + if p.relatives & flag != 0 {
                                position[i]
                            } else {
                                0.0
                            };
                    }
                    yaw = p.yaw
                        + if p.relatives & relative::Y_ROT != 0 {
                            yaw
                        } else {
                            0.0
                        };
                    pitch = p.pitch
                        + if p.relatives & relative::X_ROT != 0 {
                            pitch
                        } else {
                            0.0
                        };
                    let [x, y, z] = position;
                    session.send(S::AcceptTeleportation(play::AcceptTeleportation {
                        id: p.teleport,
                        x,
                        y,
                        z,
                        yaw,
                        pitch,
                    }));
                    level
                        .as_mut()
                        .ok_or("teleport received before Play Login")?
                        .teleported = true;
                }
                C::LevelChunkWithLight(p) => {
                    let level = level.as_mut().ok_or("chunk received before Play Login")?;
                    let biome_bits = ceil_log2(registries.len("minecraft:worldgen/biome"));
                    let sections =
                        decode_chunk_sections(&p.chunk.buffer, level.sections, biome_bits)
                            .map_err(|e| {
                                format!("chunk ({}, {}) in {}: {e}", p.x, p.z, level.name)
                            })?;
                    for section in sections {
                        remap_block_palette(section.states)?;
                    }
                    level.saw_chunk = true;
                    chunks += 1;
                }
                C::ChunkBatchFinished(_) => {
                    session.send(S::ChunkBatchReceived(play::ChunkBatchReceived {
                        chunks_per_tick: 8.0,
                    }));
                    batches += 1;
                    if let Some(level) = level.as_mut()
                        && level.teleported
                        && level.saw_chunk
                        && !level.ready
                    {
                        session.send(S::PlayerLoaded(Empty));
                        level.ready = true;
                        nether_ready |= level.name == "minecraft:the_nether";
                        println!("ready in {}", level.name);
                    }
                }
                C::PlayerInfoUpdate(p) if !peer_listed => {
                    peer_listed = p.entries.iter().any(|entry| {
                        Some(entry.uuid) == peer
                            && entry.listed == Some(true)
                            && entry
                                .name
                                .as_ref()
                                .is_some_and(|name| offline_uuid(name) == entry.uuid)
                    });
                    if peer_listed {
                        println!("peer listed");
                    }
                }
                C::PlayerInfoRemove(p)
                    if peer_listed && peer.is_some_and(|uuid| p.uuids.contains(&uuid)) =>
                {
                    peer_removed = true;
                    println!("peer removed from roster");
                }
                C::KeepAlive(_) => keepalives += 1,
                C::Ping(p) => session.send(S::Pong(p)),
                _ => {}
            }
        }
        if level.as_ref().is_some_and(|level| level.ready)
            && (!require_nether || nether_ready)
            && (peer.is_none() || (peer_listed && (require_nether || peer_removed)))
            && keepalives >= 2
        {
            println!(
                "1.21.11 join passed ({chunks} decoded and remapped chunks, {batches} batches, {keepalives} keepalives)"
            );
            return Ok(());
        }
        let now = Instant::now();
        if level.as_ref().is_some_and(|level| level.teleported) && now >= next_tick {
            session.send(S::ClientTickEnd(Empty));
            next_tick = now + Duration::from_millis(50);
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(format!("probe incomplete: peer_listed={peer_listed}, peer_removed={peer_removed}, nether_ready={nether_ready}, chunks={chunks}, batches={batches}, keepalives={keepalives}").into())
}

fn main() -> ProbeResult<()> {
    let mut args = std::env::args().skip(1);
    let address = args.next().ok_or("expected host:port [--require-nether]")?;
    let require_nether = match args.next().as_deref() {
        None => false,
        Some("--require-nether") => true,
        Some("--overworld") => false,
        Some(_) => return Err("expected --require-nether or --overworld".into()),
    };
    let name = args.next().unwrap_or_else(|| "RadioProbe".into());
    let peer = args.next().map(|name| offline_uuid(&name));
    if args.next().is_some() {
        return Err("unexpected arguments after peer name".into());
    }
    run(
        &Session::connect_radio_12111(
            &address,
            &name,
            None,
            configuration::ClientInformation::new(3),
            Arc::new(Language::new()),
            Default::default(),
        ),
        require_nether,
        peer,
    )
}
