//! First wire-format pieces for Minecraft Java 1.21.11 (protocol 774).
//!
//! These are isolated codecs and are NOT wired into Horizon's 26.3
//! connection state machine yet. In particular, the 26.3 login success
//! packet includes a session UUID that 1.21.11 does not send.

use crate::{
    buf::{Codec, Reader, Writer},
    packets::{handshake, login},
    Result,
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
        return Err(crate::Error::new("unexpected trailing bytes in 1.21.11 Login Success"));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_uses_protocol_774() {
        let packet = handshake_packet("localhost", 25565, handshake::LOGIN);
        let bytes = packet.encode();
        assert_eq!(bytes[0], 0); // Handshake packet ID
        assert_eq!(&bytes[1..3], &[0x86, 0x06]); // VarInt 774
        crate::packets::round_trip(packet, handshake::Serverbound::encode, handshake::Serverbound::decode);
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
        assert!(<login::LoginFinished as Codec<login::LoginFinished>>::read(
            &mut Reader::new(&writer.data)
        ).is_err());

        let mut with_session = writer.data;
        with_session.extend_from_slice(&[0; 16]);
        assert!(decode_login_finished(&with_session).is_err());
    }
}
