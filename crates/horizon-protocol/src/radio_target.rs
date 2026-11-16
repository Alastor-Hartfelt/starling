//! Radio Client Rust's target-version metadata.
//!
//! This is deliberately separate from `horizon_core::version`, which describes
//! the currently implemented Horizon 26.3 engine. Do not replace the engine's
//! generated constants until the corresponding codecs, registries, assets and
//! world handling have been ported and verified.
//!
//! Version metadata alone does NOT make a client protocol-compatible.

/// The Minecraft Java Edition release Radio is being ported to.
pub const TARGET_GAME_VERSION: &str = "1.21.11";
/// Minecraft Java Edition 1.21.11's network protocol number.
pub const TARGET_PROTOCOL_VERSION: i32 = 774;

/// A version pair used by Radio's compatibility work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VersionIdentity {
    pub game_version: &'static str,
    pub protocol_version: i32,
}

pub const TARGET: VersionIdentity = VersionIdentity {
    game_version: TARGET_GAME_VERSION,
    protocol_version: TARGET_PROTOCOL_VERSION,
};

/// Metadata of the inherited engine, NOT a claim of target compatibility.
pub fn inherited_engine() -> VersionIdentity {
    VersionIdentity {
        game_version: crate::GAME_VERSION,
        protocol_version: crate::PROTOCOL_VERSION,
    }
}

/// True only if the inherited engine's advertised identity matches the target.
///
/// Even when this returns true, packet/registry compatibility must be tested
/// separately; matching version numbers is not sufficient.
pub fn inherited_identity_matches_target() -> bool {
    inherited_engine() == TARGET
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radio_target_is_minecraft_1_21_11() {
        assert_eq!(TARGET.game_version, "1.21.11");
        assert_eq!(TARGET.protocol_version, 774);
    }

    #[test]
    fn inherited_engine_identity_is_reported_without_spoofing() {
        let engine = inherited_engine();
        assert_eq!(engine.game_version, crate::GAME_VERSION);
        assert_eq!(engine.protocol_version, crate::PROTOCOL_VERSION);
    }
}
