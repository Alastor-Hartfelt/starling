//! Headless offline-mode join and liveness check against a Java 1.21.11 server.
//! Run: cargo run -p horizon-protocol --example radio_12111_probe -- 127.0.0.1:25565
use horizon_protocol::{
    State,
    adapter::ServerVersion,
    buf::{Codec, Reader},
    connection::Connection,
    crypto::offline_uuid,
    ids,
    packets::{Empty, configuration as cfg, handshake, login, play},
    radio_12111::{
        decode_login_finished, decode_play_login, encode_teleport_ack, handshake_packet,
    },
    servers::parse_address,
};
use std::{
    error::Error,
    time::{Duration, Instant},
};

const VERSION: ServerVersion = ServerVersion::V1_21_11;
const IO_TIMEOUT: Duration = Duration::from_secs(15);

type ProbeResult<T> = Result<T, Box<dyn Error>>;

fn send(c: &mut Connection, state: State, body: &[u8]) -> ProbeResult<()> {
    let body = VERSION
        .serverbound_body(state, body)?
        .ok_or("packet has no 1.21.11 wire ID")?;
    Ok(c.send(&body)?)
}

fn login(c: &mut Connection, host: &str, port: u16) -> ProbeResult<()> {
    c.send(&handshake_packet(host, port, handshake::LOGIN).encode())?;
    let name = "RadioProbe";
    c.send(
        &login::Serverbound::Hello(login::ServerHello {
            name: name.into(),
            uuid: offline_uuid(name),
        })
        .encode(),
    )?;
    loop {
        let (id, body) = c.recv()?;
        if id == ids::login::clientbound::LOGIN_FINISHED {
            let profile = decode_login_finished(&body)?.profile;
            if profile.name != name || profile.uuid != offline_uuid(name) {
                return Err("server returned an unexpected offline profile".into());
            }
            c.send(&login::Serverbound::LoginAcknowledged(Empty).encode())?;
            return Ok(());
        }
        match login::Clientbound::decode(id, &body)? {
            login::Clientbound::Compression(p) => c.set_compression(p.threshold),
            login::Clientbound::Disconnect(p) => return Err(p.reason_json.into()),
            login::Clientbound::Hello(_) => return Err("probe requires offline mode".into()),
            login::Clientbound::CustomQuery(p) => {
                c.send(
                    &login::Serverbound::CustomQueryAnswer(login::CustomQueryAnswer {
                        transaction: p.transaction,
                        payload: None,
                    })
                    .encode(),
                )?;
            }
            login::Clientbound::CookieRequest(p) => {
                c.send(
                    &login::Serverbound::CookieResponse(login::CookieResponse {
                        key: p.key,
                        payload: None,
                    })
                    .encode(),
                )?;
            }
            other => return Err(format!("unexpected login packet: {other:?}").into()),
        }
    }
}

fn configure(c: &mut Connection) -> ProbeResult<()> {
    let state = State::Configuration;
    send(
        c,
        state,
        &cfg::Serverbound::ClientInformation(cfg::ClientInformation::new(3)).encode(),
    )?;
    loop {
        let (id, body) = c.recv()?;
        let Some(id) = VERSION.clientbound_id(state, id) else {
            continue;
        };
        let reply = match cfg::Clientbound::decode(id, &body)? {
            cfg::Clientbound::KeepAlive(p) => cfg::Serverbound::KeepAlive(p),
            cfg::Clientbound::Ping(p) => cfg::Serverbound::Pong(p),
            cfg::Clientbound::SelectKnownPacks(_) => {
                cfg::Serverbound::SelectKnownPacks(cfg::SelectKnownPacks { packs: vec![] })
            }
            cfg::Clientbound::CookieRequest(p) => {
                cfg::Serverbound::CookieResponse(cfg::CookieResponse {
                    key: p.key,
                    payload: None,
                })
            }
            cfg::Clientbound::Disconnect(p) => return Err(format!("{:?}", p.reason).into()),
            cfg::Clientbound::FinishConfiguration(_) => {
                return send(
                    c,
                    state,
                    &cfg::Serverbound::FinishConfiguration(Empty).encode(),
                );
            }
            _ => continue,
        };
        send(c, state, &reply.encode())?;
    }
}

fn play(c: &mut Connection) -> ProbeResult<()> {
    use ids::play::clientbound as id;

    let state = State::Play;
    let deadline = Instant::now() + Duration::from_secs(40);
    c.reader.set_read_timeout(Some(Duration::from_millis(50)))?;
    let mut joined = false;
    let mut teleported = false;
    let mut batches = 0;
    let mut keepalives = 0;
    let mut next_tick = Instant::now();
    while Instant::now() < deadline {
        let now = Instant::now();
        if teleported && now >= next_tick {
            send(c, state, &play::Serverbound::ClientTickEnd(Empty).encode())?;
            next_tick = now + Duration::from_millis(50);
        }
        let (wire_id, body) = match c.recv() {
            Ok(packet) => packet,
            Err(e) if e.is_timed_out() => continue,
            Err(e) => return Err(e.into()),
        };
        let Some(id) = VERSION.clientbound_id(state, wire_id) else {
            continue;
        };
        match id {
            id::LOGIN => {
                let login = decode_play_login(&body)?;
                println!(
                    "entered play as entity {} in {}",
                    login.player_id, login.spawn.dimension
                );
                joined = true;
            }
            id::PLAYER_POSITION => {
                let play::Clientbound::PlayerPosition(p) = play::Clientbound::decode(id, &body)?
                else {
                    unreachable!();
                };
                // This smoke test exercises a vanilla server's absolute initial teleport.
                if p.relatives != 0 {
                    return Err("probe received a relative teleport".into());
                }
                c.send(&encode_teleport_ack(p.teleport))?;
                let [x, y, z] = p.position;
                send(
                    c,
                    state,
                    &play::Serverbound::MovePlayerPosRot(play::MovePlayerPosRot {
                        x,
                        y,
                        z,
                        yaw: p.yaw,
                        pitch: p.pitch,
                        flags: 0,
                    })
                    .encode(),
                )?;
                if !teleported {
                    send(c, state, &play::Serverbound::PlayerLoaded(Empty).encode())?;
                    teleported = true;
                }
            }
            id::CHUNK_BATCH_FINISHED => {
                play::Clientbound::decode(id, &body)?;
                send(
                    c,
                    state,
                    &play::Serverbound::ChunkBatchReceived(play::ChunkBatchReceived {
                        chunks_per_tick: 8.0,
                    })
                    .encode(),
                )?;
                batches += 1;
            }
            id::KEEP_ALIVE => {
                let mut r = Reader::new(&body);
                let p = cfg::KeepAlive::read(&mut r)?;
                send(c, state, &play::Serverbound::KeepAlive(p).encode())?;
                keepalives += 1;
            }
            id::PING => {
                let mut r = Reader::new(&body);
                let p = cfg::Ping::read(&mut r)?;
                send(c, state, &play::Serverbound::Pong(p).encode())?;
            }
            id::DISCONNECT => {
                let reason = cfg::Disconnect::read(&mut Reader::new(&body))?;
                return Err(format!("{:?}", reason.reason).into());
            }
            _ => {}
        }
        if joined && teleported && batches > 0 && keepalives >= 2 {
            println!("1.21.11 join passed ({batches} chunk batches, {keepalives} keepalives)");
            return Ok(());
        }
    }
    Err(format!("join incomplete: joined={joined}, teleported={teleported}, batches={batches}, keepalives={keepalives}").into())
}

fn main() -> ProbeResult<()> {
    let address = std::env::args().nth(1).ok_or("expected host:port")?;
    let (host, port) = parse_address(&address).ok_or("invalid server address")?;
    let mut c = Connection::connect(&host, port, IO_TIMEOUT)?;
    c.reader.set_read_timeout(Some(IO_TIMEOUT))?;
    login(&mut c, &host, port).map_err(|e| format!("login: {e}"))?;
    println!("login profile accepted");
    configure(&mut c).map_err(|e| format!("configuration: {e}"))?;
    println!("configuration complete");
    play(&mut c).map_err(|e| format!("play: {e}").into())
}
