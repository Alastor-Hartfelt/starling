Warning: truncated output (original token count: 21792)
Total output lines: 2233

//! Multiplayer session: a network thread that connects to a server, logs in,
//! runs the configuration phase and then forwards decoded play packets to the
//! game thread. Keep-alives are answered on the network thread; pings wait for
//! the game thread, like ClientCommonPacketListenerImpl.handlePing, so a pong
//! cannot overtake an earlier world update or teleport acknowledgement.
//! Online-mode servers need the Microsoft account the launcher signed in
//! (crate::auth): the login joins the server through the session service and
//! encrypts the connection (ClientHandshakePacketListenerImpl.handleHello).
use crate::auth::AuthSession;
use horizon_protocol::{
    PROTOCOL_VERSION, State,
    adapter::ServerVersion,
    connection::{ConnReader, ConnWriter, Connection, Counters},
    crypto::{generate_secret, offline_uuid, server_hash},
    ids,
    packets::{Empty, configuration as cfg, handshake, login, play},
    radio_12111,
    registry::Dynamic,
    rsa::RsaPublicKey,
    servers::parse_address,
    text::{Language, plain, translatable},
};
use horizon_world::nbt::{JavaString, Tag};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    thread::JoinHandle,
    time::Duration,
};

/// `CommonListenerCookie.serverCookies`: cookies the server asked the client
/// to store (`ClientboundStoreCookiePacket`), answered to
/// `ClientboundCookieRequestPacket`. A `transfer` carries them to the next
/// connection (`TransferState`), so a proxy's backend switch keeps them.
pub type Cookies = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// What the network thread reports to the game.
pub enum Event {
    /// Progress text for the connecting screen.
    Status(String),
    /// The server's `minecraft:brand` custom payload.
    Brand(String),
    /// The server runs mods' quiet commands (`horizon_protocol::quiet`).
    QuietCommands,
    /// The server carries mod messages (`horizon_protocol::mod_net`).
    ModNet,
    /// The server samples far terrain for this client (`lod_net`).
    FarTerrain(crate::lod_net::Offer),
    /// ClientConfigurationPacketListenerImpl.handleEnabledFeatures.
    Features(Vec<String>),
    /// Configuration finished: the play state begins with these registries.
    Joined {
        registries: Arc<Dynamic>,
        uuid: u128,
        name: String,
    },
    Packet(Box<play::Clientbound>),
    /// A packet that failed to decode (skipped, like vanilla's skippable packets).
    DecodeError(String),
    /// The server sent us back to configuration (`ClientboundStartConfigurationPacket`):
    /// the game shows `ServerReconfigScreen` until the server logs it in again.
    Reconfiguring,
    /// The server sent us back to configuration and finished it again.
    Reconfigured(Arc<Dynamic>),
    /// ClientboundResourcePackPushPacket, in configuration or in play.
    PackPush(cfg::ResourcePackPush),
    /// ClientboundResourcePackPopPacket: one pack, or every pack.
    PackPop(Option<u128>),
    /// `show_dialog` in configuration: the dialog itself.
    ShowDialog(horizon_world::nbt::Tag),
    /// `clear_dialog` in configuration.
    ClearDialog,
    /// ClientboundCodeOfConductPacket: the game shows it and answers with
    /// `Session::accept_code_of_conduct` or a disconnect; until then the
    /// server waits in configuration.
    CodeOfConduct(String),
    /// `ClientboundTransferPacket`: the server sent us to another address.
    /// The connection ends right after; the game reconnects there with the
    /// stored cookies.
    Transfer {
        host: String,
        port: i32,
    },
    /// `ClientboundServerLinksPacket`: the links the pause menu's "Server
    /// Links..." lists (ClientCommonPacketListenerImpl.handleServerLinks).
    ServerLinks(Vec<cfg::ServerLink>),
    /// The connection ended. `from_server`: the server gave the reason (or
    /// the connection was refused for good), rather than the connection
    /// failing (auto-reconnect retries only failures and shutdowns).
    Disconnected {
        reason: String,
        /// The reason's translation key, when it had one
        /// (`reconnect::allowed` classifies by it).
        key: Option<String>,
        from_server: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Wire {
    Current,
    Radio12111,
}

impl Wire {
    fn version(self) -> ServerVersion {
        match self {
            Self::Current => ServerVersion::Current,
            Self::Radio12111 => ServerVersion::V1_21_11,
        }
    }

    fn send(
        self,
        writer: &mut ConnWriter,
        state: State,
        body: &[u8],
    ) -> horizon_protocol::Result<()> {
        if self == Self::Current {
            return writer.send(body);
        }
        if let Some(body) = self.version().serverbound_body(state, body)? {
            writer.send(&body)?;
        }
        Ok(())
    }

    fn send_play(
        self,
        writer: &mut ConnWriter,
        packet: play::Serverbound,
    ) -> horizon_protocol::Result<()> {
        if self == Self::Radio12111 {
            match packet {
                play::Serverbound::AcceptTeleportation(p) => {
                    writer.send(&radio_12111::encode_teleport_ack(p.id))?;
                    let position = play::Serverbound::MovePlayerPosRot(play::MovePlayerPosRot {
                        x: p.x,
                        y: p.y,
                        z: p.z,
                        yaw: p.yaw,
                        pitch: p.pitch,
                        flags: 0,
                    });
                    return self.send(writer, State::Play, &position.encode());
                }
                play::Serverbound::ContainerClick(click) => {
                    let packet = play::Serverbound::ContainerClick(
                        radio_12111::remap_container_click(click),
                    );
                    return self.send(writer, State::Play, &packet.encode());
                }
                play::Serverbound::PlayerAction(action) => {
                    if let Some(action) = radio_12111::remap_player_action(action) {
                        let body = play::Serverbound::PlayerAction(action).encode();
                        self.send(writer, State::Play, &body)?;
                    }
                    return Ok(());
                }
                play::Serverbound::KeepAlive(_)
                | play::Serverbound::Pong(_)
                | play::Serverbound::CookieResponse(_)
                | play::Serverbound::ClientTickEnd(_)
                | play::Serverbound::ChatCommand(_)
                | play::Serverbound::ContainerClose(_)
                | play::Serverbound::ChunkBatchReceived(_)
                | play::Serverbound::PlayerLoaded(_)
                | play::Serverbound::ClientInformation(_)
                | play::Serverbound::MovePlayerPos(_)
                | play::Serverbound::MovePlayerPosRot(_)
                | play::Serverbound::MovePlayerRot(_)
                | play::Serverbound::MovePlayerStatus(_)
                | play::Serverbound::PlayerInput(_)
                | play::Serverbound::SetCarriedItem(_)
                | play::Serverbound::UseItemOn(_) => {}
                // Only send the supported subset of protocol 774 play packets.
                _ => return Ok(()),
            }
        }
        self.send(writer, State::Play, &packet.encode())
    }

    fn decode_play(
        self,
        id: i32,
        payload: &[u8],
    ) -> horizon_protocol::Result<Option<play::Clientbound>> {
        use ids::play::clientbound as canonical;
        if self == Self::Current {
            return play::Clientbound::decode(id, payload).map(Some);
        }
        let packet = match id {
            canonical::LOGIN => {
                let p = radio_12111::decode_play_login(payload)?;
                play::Clientbound::Login(play::Login {
                    player_id: p.player_id,
                    hardcore: p.hardcore,
                    levels: p.levels,
                    max_players: p.max_players,
                    chunk_radius: p.chunk_radius,
                    simulation_distance: p.simulation_distance,
                    reduced_debug_info: p.reduced_debug_info,
                    show_death_screen: p.show_death_screen,
                    limited_crafting: p.limited_crafting,
                    spawn: p.spawn.into(),
                    online_mode: false, // Protocol 774 sessions currently support offline login.
                    enforces_secure_chat: p.enforces_secure_chat,
                })
            }
            canonical::RESPAWN => {
                let p = radio_12111::decode_respawn(payload)?;
                play::Clientbound::Respawn(play::Respawn {
                    spawn: p.spawn.into(),
                    keep: p.keep,
                })
            }
            canonical::LEVEL_CHUNK_WITH_LIGHT => {
                // Section palettes still contain 1.21.11 state IDs and wire bytes.
                play::Clientbound::LevelChunkWithLight(radio_12111::decode_level_chunk(payload)?)
            }
            canonical::BLOCK_UPDATE => {
                let play::Clientbound::BlockUpdate(mut p) = play::Clientbound::decode(id, payload)?
                else {
                    unreachable!();
                };
                p.state = radio_12111::block_state_to_engine(p.state).ok_or_else(|| {
                    horizon_protocol::Error::new("unknown 1.21.11 block update state")
                })?;
                play::Clientbound::BlockUpdate(p)
            }
            canonical::BLOCK_ENTITY_DATA => {
                let play::Clientbound::BlockEntityData(mut p) =
                    play::Clientbound::decode(id, payload)?
                else {
                    unreachable!();
                };
                let Some(kind) = radio_12111::block_entity_type_to_engine(p.kind) else {
                    return Ok(None);
                };
                p.kind = kind;
                play::Clientbound::BlockEntityData(p)
            }
            canonical::LIGHT_UPDATE => {
                play::Clientbound::LightUpdate(radio_12111::decode_light_update(payload)?)
            }
            canonical::SECTION_BLOCKS_UPDATE => {
                let play::Clientbound::SectionBlocksUpdate(mut p) =
                    play::Clientbound::decode(id, payload)?
                else {
                    unreachable!();
                };
                for update in &mut p.changes {
                    let state = i32::try_from(*update >> 12)
                        .ok()
                        .and_then(radio_12111::block_state_to_engine)
                        .ok_or_else(|| {
                            horizon_protocol::Error::new("unknown 1.21.11 section update state")
                        })?;
                    *update = (i64::from(state) << 12) | (*update & 0xFFF);
                }
                play::Clientbound::SectionBlocksUpdate(p)
            }
            canonical::ADD_ENTITY => {
                let play::Clientbound::AddEntity(p) = play::Clientbound::decode(id, payload)?
                else {
                    unreachable!();
                };
                play::Clientbound::AddEntity(radio_12111::remap_entity_spawn(p)?)
            }
            canonical::ENTITY_POSITION_SYNC => play::Clientbound::EntityPositionSync(
                radio_12111::decode_entity_position_sync(payload)?,
            ),
            canonical::MOVE_ENTITY_POS
            | canonical::MOVE_ENTITY_POS_ROT
            | canonical::MOVE_ENTITY_ROT => radio_12111::decode_entity_move(id, payload)?,
            canonical::CONTAINER_SET_CONTENT
            | canonical::CONTAINER_SET_SLOT
            | canonical::SET_PLAYER_INVENTORY
            | canonical::SET_CURSOR_ITEM => radio_12111::decode_inventory(id, payload)?,
            canonical::PLAYER_POSITION
            | canonical::OPEN_SCREEN
            | canonical::CONTAINER_CLOSE
            | canonical::CONTAINER_SET_DATA
            | canonical::SET_HELD_SLOT
            | canonical::BLOCK_CHANGED_ACK
            | canonical::PLAYER_INFO_UPDATE
            | canonical::PLAYER_INFO_REMOVE
            | canonical::REMOVE_ENTITIES
            | canonical::ROTATE_HEAD
            | canonical::TELEPORT_ENTITY
            | canonical::SET_ENTITY_MOTION
            | canonical::CHUNK_BATCH_FINISHED
            | canonical::FORGET_LEVEL_CHUNK
            | canonical::GAME_EVENT
            | canonical::SET_CHUNK_CACHE_CENTER
            | canonical::SET_CHUNK_CACHE_RADIUS
            | canonical::SET_SIMULATION_DISTANCE
            | canonical::PLAYER_ABILITIES
            | canonical::SYSTEM_CHAT
            | canonical::SET_HEALTH
            | canonical::KEEP_ALIVE
            | canonical::PING
            | canonical::DISCONNECT
            | canonical::START_CONFIGURATION
            | canonical::COOKIE_REQUEST
            | canonical::STORE_COOKIE => play::Clientbound::decode(id, payload)?,
            _ => return Ok(None),
        };
        Ok(Some(packet))
    }
}

pub struct Session {
    events: Receiver<Event>,
    wire: Wire,
    writer: Arc<Mutex<Option<ConnWriter>>>,
    /// The connection is in configuration (else in play). Changed only
    /// with `writer` locked, as the packet that switches is sent.
    configuring: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub address: String,
    pub name: String,
    pub counters: Arc<Mutex<Option<Arc<Counters>>>>,
    /// The cookies this server stored on us (`ClientboundStoreCookiePacket`).
    pub cookies: Cookies,
    /// HORIZON_NET_DEBUG=1 logs every packet sent from the game thread.
    debug: bool,
    /// The far terrain requests waiting for the server (`lod_net`).
    far: Arc<crate::lod_net::Inbox>,
}
impl Session {
    /// Start connecting in the background, as `auth`'s account when there is one.
    pub fn connect(
        address: &str,
        name: &str,
        auth: Option<Arc<AuthSession>>,
        info: cfg::ClientInformation,
        lang: Arc<Language>,
        resources: PathBuf,
    ) -> Self {
        Self::start(
            address,
            name,
            auth,
            info,
            lang,
            None,
            None,
            resources,
            false,
            Wire::Current,
        )
    }
    /// Experimental 1.21.11 session, optionally using the launcher account.
    pub fn connect_radio_12111(
        address: &str,
        name: &str,
        auth: Option<Arc<AuthSession>>,
        info: cfg::ClientInformation,
        lang: Arc<Language>,
        resources: PathBuf,
    ) -> Self {
        Self::start(
            address,
            name,
            auth,
            info,
            lang,
            None,
            None,
            resources,
            false,
            Wire::Radio12111,
        )
    }

    /// This session's chunk buffers use the 1.21.11 section format.
    pub fn is_radio_12111(&self) -> bool {
        self.wire == Wire::Radio12111
    }

    /// `ClientHandshakePacketListenerImpl.handleHello`: LAN entries can continue
    /// after session authentication fails; the server still decides whether to admit them.
    /// Only for entries selected from LAN discovery, not addresses classified by IP.
    pub fn connect_lan(
        address: &str,
        name: &str,
        auth: Option<Arc<AuthSession>>,
        info: cfg::ClientInformation,
        lang: Arc<Language>,
        resources: PathBuf,
    ) -> Self {
        Self::start(
            address,
            name,
            auth,
            info,
            lang,
            None,
            None,
            resources,
            true,
            Wire::Current,
        )
    }
    /// `ConnectScreen.startConnecting` for a `transfer`: the cookies the
    /// server stored carry to the new connection (`TransferState`).
    pub fn connect_transferred(
        address: &str,
        name: &str,
        auth: Option<Arc<AuthSession>>,
        info: cfg::ClientInformation,
        lang: Arc<Language>,
        cookies: Cookies,
        resources: PathBuf,
    ) -> Self {
        Self::start(
            address,
            name,
            auth,
            info,
            lang,
            None,
            Some(cookies),
            resources,
            false,
            Wire::Current,
        )
    }
    /// Join an integrated server through memory (`Connection.connectToLocalServer`).
    pub fn connect_local(
        pipe: horizon_protocol::connection::Pipe,
        name: &str,
        info: cfg::ClientInformation,
        lang: Arc<Language>,
        resources: PathBuf,
    ) -> Self {
        Self::start(
            "local",
            name,
            None,
            info,
            lang,
            Some(pipe),
            None,
            resources,
            false,
            Wire::Current,
        )
    }
    fn start(
        address: &str,
        name: &str,
        auth: Option<Arc<AuthSession>>,
        info: cfg::ClientInformation,
        lang: Arc<Language>,
        local: Option<horizon_protocol::connection::Pipe>,
        cookies: Option<Cookies>,
        resources: PathBuf,
        lan: bool,
        wire: Wire,
    ) -> Self {
        let (send, events) = channel();
        let writer = Arc::new(Mutex::new(None));
        let configuring = Arc::new(AtomicBool::new(true));
        let counters = Arc::new(Mutex::new(None));
        // Connection.initiateServerboundPlayConnection: a transfer's
        // handshake says so (ClientIntent.TRANSFER).
        let transferred = cookies.is_some();
        let cookies = cookies.unwrap_or_default();
        let (thread_writer, thread_counters, thread_cookies) =
            (writer.clone(), counters.clone(), cookies.clone());
        let thread_configuring = configuring.clone();
        let far = Arc::new(crate::lod_net::Inbox::default());
        let thread_far = far.clone();
        let (address_owned, name_owned) = (address.to_owned(), name.to_owned());
        let thread = std::thread::Builder::new()
            .name("horizon-net".into())
            .spawn(move || {
                let result = run(
                    local,
                    &address_owned,
                    &name_owned,
                    auth.as_deref(),
                    info,
                    &lang,
                    &send,
                    &thread_writer,
                    &thread_configuring,
                    &thread_counters,
                    &thread_cookies,
                    &thread_far,
                    transferred,
                    &resources,
                    lan,
                    wire,
                );
                thread_far.cancel();
                let from_server = result.is_ok();
                let why = result.unwrap_or_else(|e| e);
                if let Some(w) = thread_writer.lock().unwrap().take() {
                    w.shutdown();
                }
                let _ = send.send(Event::Disconnected {
                    reason: why.text,
                    key: why.key,
                    from_server,
                });
            })
            .ok();
        let debug = std::env::var_os("HORIZON_NET_DEBUG").is_some();
        Self {
            events,
            wire,
            writer,
            configuring,
            thread,
            address: address.to_owned(),
            name: name.to_owned(),
            counters,
            cookies,
            debug,
            far,
        }
    }
    /// Everything received since the last poll.
    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
    /// Send a play packet; silently dropped once the connection is gone,
    /// and while the server has the connection back in configuration (a
    /// proxy switching servers). Vanilla's `handleConfigurationStart` clears
    /// the level before it acknowledges, so no play packet follows the
    /// acknowledgement; one that did would reach the server as a
    /// configuration packet, and Velocity drops such a client.
    pub fn send(&self, packet: play::Serverbound) {
        if self.debug
            && !matches!(
                packet,
                play::Serverbound::ClientTickEnd(_)
                    | play::Serverbound::MovePlayerPos(_)
                    | play::Serverbound::MovePlayerPosRot(_)
            )
        {
            eprintln!("[net] -> {packet:?}");
        }
        let mut writer = self.writer.lock().unwrap();
        if self.configuring.load(Ordering::Relaxed) {
            return;
        }
        if let Some(w) = writer.as_mut()
            && self.wire.send_play(w, packet).is_err()
        {
            w.shutdown();
        }
    }
    /// Far terrain the server samples, as it offered (`Event::FarTerrain`):
    /// biomes named by this client's indices, block states below `states`.
    pub fn far_terrain(
        &self,
        offer: crate::lod_net::Offer,
        biomes: HashMap<String, u16>,
        states: u32,
    ) -> Arc<crate::lod_net::RemoteSource> {
        crate::lod_net::RemoteSource::new(self.far.clone(), offer, biomes, states)
    }
    /// The far terrain requests its samplers queued go to the server, from
    /// the game thread, among its other packets.
    pub fn send_far_terrain(&self) {
        for data in self.far.requests() {
            self.send(play::Serverbound::CustomPayload(cfg::CustomPayload {
                channel: crate::lod_net::CHANNEL.into(),
                data,
            }));
        }
    }
    /// The far terrain made with [`Self::far_terrain`] ends: its samplers
    /// stop waiting for the server.
    pub fn cancel_far_terrain(&self) {
        self.far.cancel();
    }
    /// The connection is configured again (a proxy's next server): requests
    /// the old server held never come back, so their slots are freed.
    pub fn reset_far_terrain(&self) {
        self.far.reset();
    }
    /// ServerboundResourcePackPacket, in whichever phase the connection is.
    pub fn send_pack_status(&self, id: u128, action: i32) {
        let status = cfg::ResourcePackResponse { id, action };
        let mut writer = self.writer.lock().unwrap();
        let (state, body) = if self.configuring.load(Ordering::Relaxed) {
            (
                State::Configuration,
                cfg::Serverbound::ResourcePack(status).encode(),
            )
        } else {
            (
                State::Play,
                play::Serverbound::ResourcePack(status).encode(),
            )
        };
        if let Some(w) = writer.as_mut()
            && self.wire.send(w, state, &body).is_err()
        {
            w.shutdown();
        }
    }
    /// ServerboundCustomClickActionPacket (a dialog's answer), in whichever
    /// phase the connection is.
    pub fn send_custom_click(&self, id: &str, payload: Option<horizon_world::nbt::Tag>) {
        let click = cfg::CustomClickAction {
            id: id.to_owned(),
            payload,
        };
        let mut writer = self.writer.lock().unwrap();
        let (state, body) = if self.configuring.load(Ordering::Relaxed) {
            (
                State::Configuration,
                cfg::Serverbound::CustomClickAction(click).encode(),
            )
        } else {
            (
                State::Play,
                play::Serverbound::CustomClickAction(click).encode(),
            )
        };
        if let Some(w) = writer.as_mut()
            && self.wire.send(w, state, &body).is_err()
        {
            w.shutdown();
        }
    }
    /// ServerboundAcceptCodeOfConductPacket, sent in configuration.
    pub fn accept_code_of_conduct(&self) {
        let mut writer = self.writer.lock().unwrap();
        if let Some(w) = writer.as_mut()
            && self
                .wire
                .send(
                    w,
                    State::Configuration,
                    &cfg::Serverbound::AcceptCodeOfConduct(Empty).encode(),
                )
                .is_err()
        {
            w.shutdown();
        }
    }
    /// `Connection.disconnect`: the socket closes and the game goes on at once.
    /// The reader thread is left to end on its own: on Windows a shutdown
    /// doesn't wake a read already waiting, so joining it held the window until
    /// the server dropped the connection, many seconds later.
    pub fn disconnect(&mut self) {
        if let Some(w) = self.writer.lock().unwrap().take() {
            w.shutdown();
        }
        drop(self.thread.take());
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// An address of the form `friend:K7QM-3XPD` joins a friend's world through
/// its invite code (horizon-friends): the session goes through a tunnel the
/// client reaches on a loopback port, so everything after is a plain join.
pub const FRIEND: &str = horizon_friends::ADDRESS_PREFIX;

/// An address as players and logs see it: a friend's invite code stays
/// out of it (its secret is in there).
pub fn shown_address(address: &str) -> String {
    match address.strip_prefix(FRIEND) {
        Some(code) => match horizon_friends::invite::parse(code) {
            Some(horizon_friends::invite::Parsed::Short(c)) => {
                format!("Friend's world ({}-****)", c.nameplate)
            }
            _ => "Friend's world".to_owned(),
        },
        None => address.to_owned(),
    }
}

/// The relay the player set (`friendsRelay` in horizon.txt, beside
/// options.txt), else `HORIZON_RELAY_URL`. None by default.
pub fn friends_relay_setting() -> Option<String> {
    let dir = std::env::current_dir().ok()?;
    crate::ui_theme::preference(&dir, "friendsRelay")
        .filter(|r| !r.is_empty())
        .or_else(horizon_friends::relay_url)
}
/// Remember the relay typed on Join a friend (empty forgets it).
pub fn save_friends_relay(relay: &str) {
    if let Ok(dir) = std::env::current_dir() {
        crate::ui_theme::set_preference(&dir, "friendsRelay", (!relay.is_empty()).then_some(relay));
    }
}

/// Reach the friend's world from the code (directly, or through a relay)
/// and open the tunnel: the loopback address to connect to.
fn join_friend(code: &str, lang: &Language, send: &Sender<Event>) -> Result<(String, u16), Reason> {
    use horizon_friends::{
        PathMode,
        guest::{self, GuestConfig},
    };
    status(send, &tr(lang, "horizon.multiplayer.finding_friend"));
    let tunnel = guest::join(&GuestConfig {
        code: code.to_owned(),
        relay: friends_relay_setting(),
        protocol: PROTOCOL_VERSION,
        game: horizon_protocol::GAME_VERSION.to_owned(),
        path: PathMode::from_env(),
    })
    .map_err(|e| Reason::plain(e.to_string()))?;
    status(
        send,
        &if tunnel.direct {
            tr(lang, "horizon.multiplayer.connected_friend_direct")
        } else {
            tr(lang, "horizon.multiplayer.connected_friend_relay")
        },
    );
    let (port, _) = tunnel
        .serve_loopback()
        .map_err(|e| Reason::plain(e.to_string()))?;
    Ok(("127.0.0.1".to_owned(), port))
}

fn status(send: &Sender<Event>, text: &str) {
    let _ = send.send(Event::Status(text.to_owned()));
}

/// Shown when an online-mode server asks a client without an account to authenticate.
pub const NOT_SIGNED_IN: &str = "This server is in online mode. Sign in with your Microsoft account in the Horizon launcher to join it.";

/// A translation, or the key itself (Language.getOrDefault).
fn tr(lang: &Language, key: &str) -> String {
    lang.get(key).cloned().unwrap_or_else(|| key.to_owned())
}

/// Why a connection ended: the shown text and, when the reason was a
/// translatable component or a known Horizon error, its translation key —
/// the auto-reconnect classifies by key (`reconnect::allowed`), the
/// English keyword scan of the text is the fallback for plain reasons.
#[derive(Clone)]
pub struct Reason {
    pub text: String,
    pub key: Option<String>,
}
impl Reason {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            key: None,
        }
    }
    /// A vanilla key's text in the session's language, with the key.
    fn keyed(key: &str, lang: &Language) -> Self {
        Self {
            text: tr(lang, key),
            key: Some(key.to_owned()),
        }
    }
    /// A Horizon key's text with a `%s` argument filled.
    fn keyed_arg(key: &str, lang: &Language, arg: &str) -> Self {
        let mut r = Self::keyed(key, lang);
        r.text = r.text.replacen("%s", arg, 1);
        r
    }
    /// A server's disconnect component: the resolved text and the
    /// `translate` key when the component named one (servers' literal
    /// kicks have none; their text is the server's own language).
    fn component(reason_json: &str, lang: &Language) -> Self {
        match serde_json::from_str::<serde_json::Value>(reason_json) {
            Ok(v) => Self {
                text: json_text(&v, lang),
                key: v["translate"].as_str().map(str::to_owned),
            },
            Err(_) => Self::plain(reason_json),
        }
    }
    /// The play/configuration phases' disconnect: the reason arrives as
    /// an NBT component.
    fn tag(tag: &Tag, lang: &Language) -> Self {
        let key = match tag {
            Tag::Compound(map) => map
                .get(&JavaString::from("translate"))
                .and_then(|t| match t {
                    Tag::String(s) => s.to_string().ok(),
                    _ => None,
                }),
            _ => None,
        };
        Self {
            text: plain(tag, lang),
            key,
        }
    }
}

/// `ConnectScreen.connect`'s log line, as vanilla writes it from its "Server Connector #N" thread:
/// the address as the player gave it, and the port (25565 when none was given), before any SRV
/// lookup. Other launchers read the last one from `latest.log` to offer the server again.
fn connecting_line(host: &str, port: u16) -> String {
    format!("Connecting to {host}, {port}")
}
fn log_connecting(host: &str, port: u16) {
    // ConnectScreen.UNIQUE_THREAD_ID: the first connector is #1.
    static CONNECTORS: AtomicU32 = AtomicU32::new(0);
    let n = CONNECTORS.fetch_add(1, Ordering::Relaxed) + 1;
    crate::logging::write_as(
        &format!("Server Connector #{n}"),
        log::Level::Info,
        &connecting_line(host, port),
    );
}

/// `Connection$1.initChannel`: a client socket's pipeline starts with
/// `ReadTimeoutHandler(30)`. A server sends a keep-alive every 15 s, so one
/// that sends nothing for 30 s has frozen or dropped off the network.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// `ConnectScreen.startConnecting`: the socket to a server, with `read_timeout`
/// ([`READ_TIMEOUT`]) on every read. The friends relay supplies a concrete
/// endpoint, not a Minecraft hostname, so it skips the SRV lookup. Returns the
/// resolved host and port for `ConnectScreen`'s login handshake.
fn connect_tcp(
    host: &str,
    port: u16,
    relayed: bool,
    read_timeout: Duration,
) -> Result<(Connection, String, u16), String> {
    let (c, host, port) = if relayed {
        Connection::connect(host, port, Duration::from_secs(10)).map(|c| (c, host.to_owned(), port))
    } else {
        Connection::connect_server_with_address(host, port, Duration::from_secs(10))
    }
    .map_err(|e| e.to_string())?;
    c.reader
        .set_read_timeout(Some(read_timeout))
        .map_err(|e| e.to_string())?;
    Ok((c, host, port))
}

/// `Connection.exceptionCaught`: a read that timed out ends with
/// `disconnect.timeout`, any other failure with `disconnect.genericReason`
/// ("Internal Exception: ..."); …5792 tokens truncated…o_string();
    c.send(
        &handshake::Serverbound::Intention(handshake::Intention {
            protocol: PROTOCOL_VERSION,
            host: host.clone(),
            port,
            intent: handshake::STATUS,
        })
        .encode(),
    )
    .map_err(e)?;
    c.send(&status::Serverbound::StatusRequest(Empty).encode())
        .map_err(e)?;
    let (id, payload) = c.recv().map_err(e)?;
    let json = match status::Clientbound::decode(id, &payload).map_err(e)? {
        status::Clientbound::StatusResponse(s) => {
            serde_json::from_str(&s.json).map_err(|e| e.to_string())?
        }
        _ => return Err("unexpected status reply".into()),
    };
    let start = std::time::Instant::now();
    c.send(&status::Serverbound::PingRequest(status::Ping { time: 1 }).encode())
        .map_err(e)?;
    let _ = c.recv().map_err(e)?;
    Ok((json, start.elapsed().as_millis()))
}

/// What a server that answers only the legacy ping reports
/// (`LegacyServerPinger.Output.handleResponse`).
#[derive(Clone, Debug, PartialEq)]
pub struct LegacyStatus {
    pub protocol: i32,
    pub version: String,
    pub motd: String,
    pub players: i32,
    pub max_players: i32,
}

/// `Mth.getInt(str, default)`.
fn legacy_int(s: &str, default: i32) -> i32 {
    s.parse().unwrap_or(default)
}

/// `LegacyServerPinger.channelRead0` on the string after the 0xFF byte: split
/// on NUL (at most 6 parts) and the first part must be `§1`.
fn parse_legacy_status(text: &str) -> Option<LegacyStatus> {
    let parts: Vec<&str> = text.splitn(6, '\0').collect();
    if parts.first() != Some(&"\u{a7}1") || parts.len() < 6 {
        return None;
    }
    Some(LegacyStatus {
        protocol: legacy_int(parts[1], 0),
        version: parts[2].to_owned(),
        motd: parts[3].to_owned(),
        players: legacy_int(parts[4], -1),
        max_players: legacy_int(parts[5], -1),
    })
}

/// `LegacyProtocolUtils.writeLegacyString`: the UTF-16 length, then UTF-16BE.
fn write_legacy_string(out: &mut Vec<u8>, s: &str) {
    let units: Vec<u16> = s.encode_utf16().collect();
    out.extend_from_slice(&(units.len() as u16).to_be_bytes());
    for unit in units {
        out.extend_from_slice(&unit.to_be_bytes());
    }
}

/// `LegacyServerPinger.channelActive`: the 1.6 `0xFE 0x01 0xFA MC|PingHost` request.
fn legacy_ping_request(host: &str, port: u16) -> Vec<u8> {
    let mut out = vec![254, 1, 250];
    write_legacy_string(&mut out, "MC|PingHost");
    let mut payload = vec![127];
    write_legacy_string(&mut payload, host);
    payload.extend_from_slice(&i32::from(port).to_be_bytes());
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

/// `ServerStatusPinger.pingLegacyServer`: ask a server that does not answer
/// the status handshake with the pre-1.7 ping.
pub fn ping_legacy(address: &str, timeout: Duration) -> Result<LegacyStatus, String> {
    let (host, port) = parse_address(address).ok_or("Invalid address")?;
    let mut c = Connection::connect_server(&host, port, timeout).map_err(|e| e.to_string())?;
    c.reader
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    let e = |e: horizon_protocol::Error| e.to_string();
    c.writer
        .write_raw(&legacy_ping_request(&host, port))
        .map_err(e)?;
    let head = c.reader.read_raw(3).map_err(e)?;
    if head[0] != 255 {
        return Err("unexpected legacy reply".into());
    }
    let units = usize::from(u16::from_be_bytes([head[1], head[2]]));
    let bytes = c.reader.read_raw(units * 2).map_err(e)?;
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u16::from_be_bytes(*p))
        .collect();
    parse_legacy_status(&String::from_utf16_lossy(&units))
        .ok_or_else(|| "unexpected legacy reply".to_owned())
}

/// `ClientHandshakePacketListenerImpl.handleHello`: a failed session join goes
/// on to encryption only for an entry picked from LAN discovery
/// (`ServerData.isLan`), and the host decides. A vanilla client always has an
/// account; Horizon without one never joins anything (Mojang's rules: no play
/// without an account that owns the game), LAN included.
fn continues_after_failed_join(lan: bool, signed_in: bool) -> bool {
    lan && signed_in
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Launchers find the last server joined in `latest.log` by vanilla's line.
    #[test]
    fn radio_section_updates_remap_states_and_preserve_their_coordinates() {
        use horizon_protocol::buf::Reader;

        let raw = play::Clientbound::SectionBlocksUpdate(play::SectionBlocksUpdate {
            section: [-2, 4, 5],
            changes: vec![(279 << 12) | 0xF71, 1 << 12],
        })
        .encode();
        let mut body = Reader::new(&raw);
        let id = body.varint().unwrap();
        let packet = Wire::Radio12111
            .decode_play(id, body.rest())
            .unwrap()
            .unwrap();
        let play::Clientbound::SectionBlocksUpdate(update) = packet else {
            panic!("expected section block update");
        };
        assert_eq!(update.section, [-2, 4, 5]);
        // Default oak leaves move from 279 to 294; stone stays at 1.
        assert_eq!(update.changes, [(294 << 12) | 0xF71, 1 << 12]);
    }

    #[test]
    fn the_connecting_line_is_vanillas() {
        let line = |address: &str| {
            let (host, port) = parse_address(address).unwrap();
            connecting_line(&host, port)
        };
        assert_eq!(
            line("play.example.net"),
            "Connecting to play.example.net, 25565"
        );
        assert_eq!(
            line("play.example.net:25577"),
            "Connecting to play.example.net, 25577"
        );
        assert_eq!(line("[::1]:25566"), "Connecting to ::1, 25566");
    }

    #[test]
    fn direct_login_keeps_the_hostname_in_the_handshake() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let session = Session::connect(
            &format!("localhost:{port}"),
            "Probe",
            None,
            cfg::ClientInformation::new(8),
            Arc::new(Language::new()),
            PathBuf::new(),
        );
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut server = Connection::from_stream(stream).unwrap();
        let (id, body) = server.recv().unwrap();
        let handshake::Serverbound::Intention(packet) =
            handshake::Serverbound::decode(id, &body).unwrap()
        else {
            panic!("expected login handshake")
        };
        assert_eq!((packet.host.as_str(), packet.port), ("localhost", port));
        assert_eq!(packet.intent, handshake::LOGIN);
        drop(server);
        drop(session);
    }

    #[test]
    fn legacy_ping_request_is_the_1_6_ping_host_packet() {
        let bytes = legacy_ping_request("a.b", 25565);
        assert_eq!(&bytes[..3], &[254, 1, 250]);
        // "MC|PingHost": 11 chars.
        assert_eq!(&bytes[3..5], &[0, 11]);
        let after = 5 + 22;
        // payload: 127, string (2 + 6 bytes), port.
        assert_eq!(&bytes[after..after + 2], &[0, 1 + 2 + 6 + 4]);
        assert_eq!(bytes[after + 2], 127);
        assert_eq!(&bytes[after + 3..after + 5], &[0, 3]);
        assert_eq!(&bytes[bytes.len() - 4..], &25565i32.to_be_bytes());
    }

    #[test]
    fn legacy_status_is_split_on_nul_after_the_section_one_marker() {
        let status =
            parse_legacy_status("\u{a7}1\x00127\x001.6.4\x00A \u{a7}aserver\x003\x0020").unwrap();
        assert_eq!(
            status,
            LegacyStatus {
                protocol: 127,
                version: "1.6.4".into(),
                motd: "A \u{a7}aserver".into(),
                players: 3,
                max_players: 20,
            }
        );
        // Unparsable counts are -1, a wrong marker is no status.
        let status = parse_legacy_status("\u{a7}1\x00x\x00v\x00m\x00?\x00?").unwrap();
        assert_eq!(
            (status.protocol, status.players, status.max_players),
            (0, -1, -1)
        );
        assert!(parse_legacy_status("kick\x00127\x00v\x00m\x001\x002").is_none());
    }

    /// The en_us text of the disconnect reasons.
    fn disconnect_language() -> Language {
        [
            ("disconnect.timeout", "Timed out"),
            ("disconnect.endOfStream", "End of stream"),
            ("disconnect.genericReason", "%s"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
    }

    #[test]
    fn a_server_that_stops_answering_times_out() {
        // The server accepts and then sends nothing, like a frozen server
        // or a network gone without a reset.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let held = std::thread::spawn(move || listener.accept().map(|(stream, _)| stream));
        let (c, _, _) = connect_tcp("127.0.0.1", port, true, Duration::from_millis(300)).unwrap();
        let _server = held.join().unwrap().unwrap();
        let (mut reader, writer) = c.split();
        let (done, result) = channel();
        std::thread::spawn(move || {
            let (send, _events) = channel();
            let _ = done.send(receive_play(
                &mut reader,
                &Mutex::new(Some(writer)),
                &AtomicBool::new(false),
                &disconnect_language(),
                &send,
                &Cookies::default(),
                &crate::lod_net::Inbox::default(),
                Path::new("."),
                Wire::Current,
            ));
        });
        let ended = result
            .recv_timeout(Duration::from_secs(10))
            .expect("the connection never timed out");
        assert_eq!(
            ended.map(|r| r.text).map_err(|r| r.text),
            Err("Timed out".to_owned())
        );
    }

    #[test]
    fn lost_connections_read_like_vanillas_disconnect_reasons() {
        let lang = disconnect_language();
        let lost = |e| connection_lost(e, &lang);
        assert_eq!(lost(horizon_protocol::Error::timed_out()), "Timed out");
        assert_eq!(
            lost(horizon_protocol::Error::end_of_stream()),
            "End of stream"
        );
        assert_eq!(
            lost(horizon_protocol::Error::new("Bad VarInt")),
            "Internal Exception: Bad VarInt"
        );
    }

    #[test]
    fn configuration_answers_the_content_identity_challenge() {
        let root =
            std::env::temp_dir().join(format!("horizon-content-identity-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("facts.json"),
            br#"{"states":[["minecraft:stone",false,false,"Block{minecraft:stone}",{"id":"minecraft:stone"}]]}"#,
        )
        .unwrap();
        let expected = crate::content_identity::identity(&root).unwrap();
        let (client, server) = horizon_protocol::connection::Pipe::pair();
        let (mut reader, writer) = Connection::local(client).split();
        let writer = Mutex::new(Some(writer));
        let thread = std::thread::spawn(move || {
            let (send, _events) = channel();
            configure(
                &mut reader,
                &writer,
                &AtomicBool::new(true),
                &Language::new(),
                &send,
                &Cookies::default(),
                &root,
                Wire::Current,
            )
        });
        let mut server = Connection::local(server);
        server
            .send(
                &cfg::Clientbound::CustomPayload(cfg::CustomPayload {
                    channel: crate::content_identity::CHANNEL.into(),
                    data: vec![],
                })
                .encode(),
            )
            .unwrap();
        server
            .send(&cfg::Clientbound::Ping(cfg::Ping { id: 17 }).encode())
            .unwrap();
        let (id, body) = server.recv().unwrap();
        assert_eq!(
            cfg::Serverbound::decode(id, &body).unwrap(),
            cfg::Serverbound::CustomPayload(cfg::CustomPayload {
                channel: crate::content_identity::CHANNEL.into(),
                data: expected.to_vec(),
            })
        );
        let (id, body) = server.recv().unwrap();
        assert_eq!(
            cfg::Serverbound::decode(id, &body).unwrap(),
            cfg::Serverbound::Pong(cfg::Ping { id: 17 })
        );
        server
            .send(&cfg::Clientbound::FinishConfiguration(Empty).encode())
            .unwrap();
        let (id, body) = server.recv().unwrap();
        assert_eq!(
            cfg::Serverbound::decode(id, &body).unwrap(),
            cfg::Serverbound::FinishConfiguration(Empty)
        );
        assert!(thread.join().unwrap().is_ok());
        std::fs::remove_dir_all(
            std::env::temp_dir().join(format!("horizon-content-identity-{}", std::process::id())),
        )
        .unwrap();
    }

    #[test]
    fn disconnect_and_drop_do_not_wait_for_a_stalled_reader() {
        // A reader can remain stalled after Windows socket shutdown. It must
        // not delay either an explicit leave or Session's Drop on the UI thread.
        for explicit_disconnect in [true, false] {
            let (client, _server) = horizon_protocol::connection::Pipe::pair();
            let (_reader, writer) = Connection::local(client).split();
            let writer = Arc::new(Mutex::new(Some(writer)));
            let (_send, events) = channel();
            let (release, stalled) = channel();
            let (finished, completed) = channel();
            let thread = std::thread::spawn(move || {
                let _ = stalled.recv_timeout(std::time::Duration::from_secs(3));
                let _ = finished.send(());
            });
            let mut session = Session {
                events,
                wire: Wire::Current,
                writer: writer.clone(),
                configuring: Arc::new(AtomicBool::new(false)),
                thread: Some(thread),
                address: "local".into(),
                name: "TestPlayer".into(),
                counters: Arc::new(Mutex::new(None)),
                cookies: Cookies::default(),
                debug: false,
                far: Arc::default(),
            };
            let started = std::time::Instant::now();
            if explicit_disconnect {
                session.disconnect();
                session.disconnect(); // Repeated cleanup is harmless.
            }
            drop(session);
            let elapsed = started.elapsed();
            let reader_still_pending = completed.try_recv().is_err();
            // Always release the synthetic reader before asserting, even if
            // a future regression restores the blocking join.
            let _ = release.send(());
            if reader_still_pending {
                completed
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
            }
            assert!(
                writer.lock().unwrap().is_none(),
                "connection was not closed"
            );
            assert!(
                reader_still_pending,
                "disconnect waited for the stalled reader"
            );
            assert!(
                elapsed < std::time::Duration::from_secs(1),
                "disconnect took {elapsed:?}"
            );
        }
    }

    #[test]
    fn pings_wait_behind_teleports_but_keepalives_do_not_wait_for_the_game() {
        let (client, server) = horizon_protocol::connection::Pipe::pair();
        let (mut reader, writer) = Connection::local(client).split();
        let writer = Arc::new(Mutex::new(Some(writer)));
        let (send, events) = channel();
        let thread_writer = writer.clone();
        let thread = std::thread::spawn(move || {
            receive_play(
                &mut reader,
                &thread_writer,
                &AtomicBool::new(false),
                &Language::new(),
                &send,
                &Cookies::default(),
                &crate::lod_net::Inbox::default(),
                Path::new("."),
                Wire::Current,
            )
        });
        let mut server = Connection::local(server);
        server
            .reader
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let teleport = play::Clientbound::PlayerPosition(play::PlayerPosition {
            teleport: 42,
            position: [10.0, 64.0, 10.0],
            delta: [0.0; 3],
            yaw: 90.0,
            pitch: 0.0,
            relatives: 0,
        });
        server.send(&teleport.encode()).unwrap();
        server
            .send(&play::Clientbound::Ping(cfg::Ping { id: 7 }).encode())
            .unwrap();
        // A reply to this proves the receive loop has passed the ping. The
        // old implementation instead replies with Pong before this keepalive.
        server
            .send(&play::Clientbound::KeepAlive(cfg::KeepAlive { id: 9 }).encode())
            .unwrap();
        let (id, body) = server.recv().unwrap();
        assert_eq!(
            play::Serverbound::decode(id, &body).unwrap(),
            play::Serverbound::KeepAlive(cfg::KeepAlive { id: 9 })
        );
        let timeout = Duration::from_secs(5);
        assert!(
            matches!(events.recv_timeout(timeout).unwrap(), Event::Packet(p) if *p == teleport)
        );
        assert!(
            matches!(events.recv_timeout(timeout).unwrap(), Event::Packet(p) if *p == play::Clientbound::Ping(cfg::Ping { id: 7 }))
        );
        writer.lock().unwrap().take().unwrap().shutdown();
        assert!(thread.join().unwrap().is_err());
    }

    #[test]
    fn configuration_keepalive_bypasses_game_while_play_packets_wait() {
        let (client, server) = horizon_protocol::connection::Pipe::pair();
        let (mut reader, writer) = Connection::local(client).split();
        let (send, events) = channel();
        let session = Session {
            events,
            wire: Wire::Current,
            writer: Arc::new(Mutex::new(Some(writer))),
            configuring: Arc::new(AtomicBool::new(false)),
            thread: None,
            address: "local".into(),
            name: "Player".into(),
            counters: Arc::new(Mutex::new(None)),
            cookies: Cookies::default(),
            debug: false,
            far: Arc::default(),
        };
        let (thread_writer, thread_configuring) =
            (session.writer.clone(), session.configuring.clone());
        let thread = std::thread::spawn(move || {
            receive_play(
                &mut reader,
                &thread_writer,
                &thread_configuring,
                &Language::new(),
                &send,
                &Cookies::default(),
                &crate::lod_net::Inbox::default(),
                Path::new("."),
                Wire::Current,
            )
        });
        let mut server = Connection::local(server);
        server
            .reader
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let tick_end = play::Serverbound::ClientTickEnd(Empty);
        server
            .send(&play::Clientbound::StartConfiguration(Empty).encode())
            .unwrap();
        let (id, body) = server.recv().unwrap();
        assert_eq!(
            play::Serverbound::decode(id, &body).unwrap(),
            play::Serverbound::ConfigurationAcknowledged(Empty)
        );
        // The game thread can stall or keep ticking the old level. The read
        // thread still answers in the current phase, without any event polls.
        session.send(tick_end.clone());
        let keepalive = cfg::KeepAlive { id: i64::MIN + 42 };
        server
            .send(&cfg::Clientbound::KeepAlive(keepalive.clone()).encode())
            .unwrap();
        let (id, body) = server.recv().unwrap();
        assert_eq!(
            cfg::Serverbound::decode(id, &body).unwrap(),
            cfg::Serverbound::KeepAlive(keepalive)
        );
        server
            .send(&cfg::Clientbound::FinishConfiguration(Empty).encode())
            .unwrap();
        let (id, body) = server.recv().unwrap();
        assert_eq!(
            cfg::Serverbound::decode(id, &body).unwrap(),
            cfg::Serverbound::FinishConfiguration(Empty)
        );
        // Back in play, the game's packets flow again.
        session.send(tick_end.clone());
        let (id, body) = server.recv().unwrap();
        assert_eq!(play::Serverbound::decode(id, &body).unwrap(), tick_end);
        drop(session);
        assert!(thread.join().unwrap().is_err());
    }

    #[test]
    fn play_answers_cookie_requests_from_stored_cookies_and_follows_transfers() {
        let (client, server) = horizon_protocol::connection::Pipe::pair();
        let (mut reader, writer) = Connection::local(client).split();
        let writer = Arc::new(Mutex::new(Some(writer)));
        let (send, events) = channel();
        let cookies = Cookies::default();
        let (thread_writer, thread_cookies) = (writer.clone(), cookies.clone());
        let thread = std::thread::spawn(move || {
            receive_play(
                &mut reader,
                &thread_writer,
                &AtomicBool::new(false),
                &Language::new(),
                &send,
                &thread_cookies,
                &crate::lod_net::Inbox::default(),
                Path::new("."),
                Wire::Current,
            )
        });
        let mut server = Connection::local(server);
        let ask = |server: &mut Connection, key: &str| {
            let request = play::Clientbound::CookieRequest(cfg::CookieRequest { key: key.into() });
            server.send(&request.encode()).unwrap();
            let (id, body) = server.recv().unwrap();
            play::Serverbound::decode(id, &body).unwrap()
        };
        // handleRequestCookie answers an unknown key with no payload.
        assert_eq!(
            ask(&mut server, "proxy:session"),
            play::Serverbound::CookieResponse(cfg::CookieResponse {
                key: "proxy:session".into(),
                payload: None,
            })
        );
        let store = play::Clientbound::StoreCookie(cfg::StoreCookie {
            key: "proxy:session".into(),
            payload: vec![1, 2, 3],
        });
        server.send(&store.encode()).unwrap();
        assert_eq!(
            ask(&mut server, "proxy:session"),
            play::Serverbound::CookieResponse(cfg::CookieResponse {
                key: "proxy:session".into(),
                payload: Some(vec![1, 2, 3]),
            })
        );
        let transfer = play::Clientbound::Transfer(cfg::Transfer {
            host: "example.net".into(),
            port: 25566,
        });
        server.send(&transfer.encode()).unwrap();
        assert!(thread.join().unwrap().is_ok());
        assert!(matches!(
            events.try_iter().last(),
            Some(Event::Transfer { host, port: 25566 }) if host == "example.net"
        ));
        // The stored cookie stays for the next connection (TransferState).
        assert_eq!(
            cookies.lock().unwrap().get("proxy:session"),
            Some(&vec![1, 2, 3])
        );
    }

    #[test]
    fn server_brand_payload_is_decoded_and_invalid_or_unrelated_payloads_are_ignored() {
        let (send, events) = channel();
        let mut writer = horizon_protocol::buf::Writer::new();
        writer.string("vanilla");
        let mut payload = cfg::CustomPayload {
            channel: "minecraft:brand".into(),
            data: writer.data,
        };
        receive_brand(&payload, &send);
        assert!(matches!(events.try_recv(), Ok(Event::Brand(brand)) if brand == "vanilla"));
        payload.channel = "example:other".into();
        receive_brand(&payload, &send);
        assert!(events.try_recv().is_err());
        payload.channel = "minecraft:brand".into();
        payload.data = vec![0xff];
        receive_brand(&payload, &send);
        assert!(events.try_recv().is_err());
    }
    #[test]
    fn radio_login_handles_an_encrypted_server_challenge() {
        use horizon_protocol::buf::{Codec, Writer};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(25)))
                .unwrap();
            let mut server = Connection::from_stream(stream).unwrap();
            let (id, payload) = server.recv().unwrap();
            let handshake::Serverbound::Intention(intention) =
                handshake::Serverbound::decode(id, &payload).unwrap()
            else {
                panic!("expected the 1.21.11 handshake");
            };
            assert_eq!(intention.protocol, 774);
            let (id, payload) = server.recv().unwrap();
            let login::Serverbound::Hello(hello) =
                login::Serverbound::decode(id, &payload).unwrap()
            else {
                panic!("expected the 1.21.11 login hello");
            };
            let key = horizon_protocol::rsa::RsaPrivateKey::generate(1024).unwrap();
            let challenge = vec![10, 29, 87, 43];
            server
                .send(
                    &login::Clientbound::Hello(login::Hello {
                        server_id: String::new(),
                        public_key: key.public_der(),
                        challenge: challenge.clone(),
                        should_authenticate: false,
                    })
                    .encode(),
                )
                .unwrap();

            let (id, payload) = server.recv().unwrap();
            let login::Serverbound::Key(reply) = login::Serverbound::decode(id, &payload).unwrap()
            else {
                panic!("expected the 1.21.11 encryption reply");
            };
            assert_eq!(key.decrypt(&reply.challenge).unwrap(), challenge);
            let secret: [u8; 16] = key.decrypt(&reply.key).unwrap().try_into().unwrap();
            server.enable_encryption(&secret);
            server
                .send(
                    &login::Clientbound::Compression(login::Compression { threshold: 1 }).encode(),
                )
                .unwrap();
            server.set_compression(1);
            let mut finished = Writer::new();
            finished.varint(ids::login::clientbound::LOGIN_FINISHED);
            login::GameProfile::write(
                &login::GameProfile {
                    uuid: hello.uuid,
                    name: hello.name,
                    properties: Vec::new(),
                },
                &mut finished,
            );
            server.send(&finished.data).unwrap();
            let (id, payload) = server.recv().unwrap();
            assert!(matches!(
                login::Serverbound::decode(id, &payload).unwrap(),
                login::Serverbound::LoginAcknowledged(_)
            ));
        });
        let session = Session::connect_radio_12111(
            &address.to_string(),
            "RadioFixture",
            None,
            cfg::ClientInformation::new(3),
            Arc::new(Language::new()),
            PathBuf::new(),
        );
        worker.join().unwrap();
        drop(session);
    }

    /// No account never joins; the signed-in rule is compared with the real
    /// client login listener by test_lan_auth_parity.py.
    #[test]
    fn lan_login_continues_after_authentication_failure() {
        for lan in [true, false] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let worker = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                // Generous: a debug build's key generation on a busy CI runner takes seconds. A pass
                // never waits for these.
                stream
                    .set_read_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
                let mut server = Connection::from_stream(stream).unwrap();
                server.recv().unwrap(); // intention
                server.recv().unwrap(); // profile
                let key = horizon_protocol::rsa::RsaPrivateKey::generate(1024).unwrap();
                let challenge = vec![1, 2, 3, 4];
                server
                    .send(
                        &login::Clientbound::Hello(login::Hello {
                            server_id: String::new(),
                            public_key: key.public_der(),
                            challenge: challenge.clone(),
                            should_authenticate: true,
                        })
                        .encode(),
                    )
                    .unwrap();
                let Ok((id, body)) = server.recv() else {
                    return false;
                };
                let login::Serverbound::Key(reply) = login::Serverbound::decode(id, &body).unwrap()
                else {
                    panic!("client did not send encryption reply")
                };
                assert_eq!(key.decrypt(&reply.challenge).unwrap(), challenge);
                let secret: [u8; 16] = key.decrypt(&reply.key).unwrap().try_into().unwrap();
                server.enable_encryption(&secret);
                server
                    .send(
                        &login::Clientbound::Disconnect(login::Disconnect {
                            reason_json: r#"{"text":"LAN fixture accepted"}"#.into(),
                        })
                        .encode(),
                    )
                    .unwrap();
                true
            });
            let address = format!("127.0.0.1:{port}");
            let connect = if lan {
                Session::connect_lan
            } else {
                Session::connect
            };
            // The session's language carries Horizon's bundled strings
            // (`load_language`), so the keyed refusal resolves.
            let lang: Language = [(
                "horizon.multiplayer.not_signed_in".to_owned(),
                NOT_SIGNED_IN.to_owned(),
            )]
            .into_iter()
            .collect();
            let session = connect(
                &address,
                "Tester",
                None,
                cfg::ClientInformation::new(8),
                Arc::new(lang),
                PathBuf::new(),
            );
            let reason = loop {
                if let Event::Disconnected { reason, .. } = session
                    .events
                    .recv_timeout(Duration::from_secs(60))
                    .unwrap()
                {
                    break reason;
                }
            };
            // Without an account nothing joins, LAN or not.
            let sent_key = worker.join().unwrap();
            assert!(!sent_key, "lan {lan}: joined without an account");
            assert_eq!(reason, NOT_SIGNED_IN);
            // A signed-in account whose join the session service rejects:
            // the case vanilla's listener is run with.
            let goes_on = continues_after_failed_join(lan, true);
            assert_eq!(goes_on, lan);
            println!(
                "lan_auth={}:{}:{}",
                if lan { "LAN" } else { "OTHER" },
                goes_on,
                !goes_on
            );
        }
    }
}
