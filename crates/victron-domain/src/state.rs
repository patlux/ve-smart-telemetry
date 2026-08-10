//! Bounded device state enums.
//!
//! State codes are domain-level and wire-independent. The numeric code
//! mapping below follows the reverse-engineered VictronConnect `STATE_NAMES`
//! table documented in `analysis/victronconnect-protocol-reference.md` and
//! implemented by the Rust VREG decoder. Unknown numeric codes
//! are preserved safely as `Unknown(u8)` instead of being dropped or
//! mislabeled.

/// Charger (battery) state of a Victron solar charger.
///
/// Known codes (from the decompiled VictronConnect state table):
///
/// | code | state |
/// |-----:|-------|
/// | 0    | `Off` |
/// | 2    | `Fault` |
/// | 3    | `Bulk` |
/// | 4    | `Absorption` |
/// | 5    | `Float` |
/// | 6    | `Storage` |
/// | 7    | `Equalize` |
/// | 245  | `StartingUp` |
/// | 247  | `AutoRecondition` |
/// | 252  | `ExternalControl` |
///
/// Any other code (e.g. 1, 8, 255) is preserved as [`ChargerState::Unknown`]
/// and can be read back with [`ChargerState::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChargerState {
    /// Off (0).
    Off,
    /// Fault (2).
    Fault,
    /// Bulk charging (3).
    Bulk,
    /// Absorption charging (4).
    Absorption,
    /// Float charging (5).
    Float,
    /// Storage / float-sustain (6).
    Storage,
    /// Equalization (7).
    Equalize,
    /// Starting up (245).
    StartingUp,
    /// Auto equalize/Recondition (247).
    AutoRecondition,
    /// External control (252).
    ExternalControl,
    /// An unknown numeric code, preserved exactly.
    Unknown(u8),
}

impl ChargerState {
    /// Map a numeric wire code to a state; unknown codes become [`ChargerState::Unknown`].
    pub const fn from_code(code: u8) -> Self {
        match code {
            0 => ChargerState::Off,
            2 => ChargerState::Fault,
            3 => ChargerState::Bulk,
            4 => ChargerState::Absorption,
            5 => ChargerState::Float,
            6 => ChargerState::Storage,
            7 => ChargerState::Equalize,
            245 => ChargerState::StartingUp,
            247 => ChargerState::AutoRecondition,
            252 => ChargerState::ExternalControl,
            other => ChargerState::Unknown(other),
        }
    }

    /// The numeric wire code of this state.
    pub const fn code(self) -> u8 {
        match self {
            ChargerState::Off => 0,
            ChargerState::Fault => 2,
            ChargerState::Bulk => 3,
            ChargerState::Absorption => 4,
            ChargerState::Float => 5,
            ChargerState::Storage => 6,
            ChargerState::Equalize => 7,
            ChargerState::StartingUp => 245,
            ChargerState::AutoRecondition => 247,
            ChargerState::ExternalControl => 252,
            ChargerState::Unknown(code) => code,
        }
    }

    /// A short human-readable label (the app's spelling).
    pub const fn label(self) -> &'static str {
        match self {
            ChargerState::Off => "Off",
            ChargerState::Fault => "Fault",
            ChargerState::Bulk => "Bulk",
            ChargerState::Absorption => "Absorption",
            ChargerState::Float => "Float",
            ChargerState::Storage => "Storage",
            ChargerState::Equalize => "Equalize",
            ChargerState::StartingUp => "Starting-up",
            ChargerState::AutoRecondition => "Auto equalize/Recondition",
            ChargerState::ExternalControl => "External control",
            ChargerState::Unknown(_) => "Unknown",
        }
    }
}

/// Load output state of a Victron solar charger.
///
/// | code | state |
/// |-----:|-------|
/// | 0    | `Off` |
/// | 1    | `On` |
///
/// Unknown codes (e.g. 2, 255) are preserved as [`LoadState::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoadState {
    /// Load output off (0).
    Off,
    /// Load output on (1).
    On,
    /// An unknown numeric code, preserved exactly.
    Unknown(u8),
}

impl LoadState {
    /// Map a numeric wire code to a state; unknown codes become [`LoadState::Unknown`].
    pub const fn from_code(code: u8) -> Self {
        match code {
            0 => LoadState::Off,
            1 => LoadState::On,
            other => LoadState::Unknown(other),
        }
    }

    /// The numeric wire code of this state.
    pub const fn code(self) -> u8 {
        match self {
            LoadState::Off => 0,
            LoadState::On => 1,
            LoadState::Unknown(code) => code,
        }
    }

    /// A short human-readable label.
    pub const fn label(self) -> &'static str {
        match self {
            LoadState::Off => "Off",
            LoadState::On => "On",
            LoadState::Unknown(_) => "Unknown",
        }
    }
}

/// BLE connection health of the collector against its device.
///
/// Finer operational states (discovering, reconnecting, contended by
/// VictronConnect) are a service-layer concern and intentionally not modeled
/// here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionHealth {
    /// The BLE connection is up and samples are arriving.
    Up,
    /// The BLE connection is down.
    Down,
    /// Health has not been determined yet.
    Unknown,
}

impl ConnectionHealth {
    /// A short human-readable label.
    pub const fn label(self) -> &'static str {
        match self {
            ConnectionHealth::Up => "Up",
            ConnectionHealth::Down => "Down",
            ConnectionHealth::Unknown => "Unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charger_state_known_codes() {
        assert_eq!(ChargerState::from_code(0), ChargerState::Off);
        assert_eq!(ChargerState::from_code(2), ChargerState::Fault);
        assert_eq!(ChargerState::from_code(3), ChargerState::Bulk);
        assert_eq!(ChargerState::from_code(4), ChargerState::Absorption);
        assert_eq!(ChargerState::from_code(5), ChargerState::Float);
        assert_eq!(ChargerState::from_code(6), ChargerState::Storage);
        assert_eq!(ChargerState::from_code(7), ChargerState::Equalize);
        assert_eq!(ChargerState::from_code(245), ChargerState::StartingUp);
        assert_eq!(ChargerState::from_code(247), ChargerState::AutoRecondition);
        assert_eq!(ChargerState::from_code(252), ChargerState::ExternalControl);
    }

    #[test]
    fn charger_state_unknown_codes_preserved() {
        for code in [1u8, 8, 9, 100, 200, 254, 255] {
            let state = ChargerState::from_code(code);
            assert!(matches!(state, ChargerState::Unknown(c) if c == code));
            assert_eq!(state.code(), code);
        }
    }

    #[test]
    fn charger_state_code_round_trip() {
        for code in [0u8, 2, 3, 4, 5, 6, 7, 245, 247, 252] {
            assert_eq!(ChargerState::from_code(code).code(), code);
        }
        for state in [
            ChargerState::Off,
            ChargerState::Bulk,
            ChargerState::StartingUp,
            ChargerState::Unknown(33),
        ] {
            assert_eq!(ChargerState::from_code(state.code()), state);
        }
    }

    #[test]
    fn charger_state_labels() {
        assert_eq!(ChargerState::Bulk.label(), "Bulk");
        assert_eq!(ChargerState::ExternalControl.label(), "External control");
        assert_eq!(ChargerState::Unknown(9).label(), "Unknown");
        assert_eq!(ChargerState::StartingUp.label(), "Starting-up");
    }

    #[test]
    fn load_state_known_and_unknown() {
        assert_eq!(LoadState::from_code(0), LoadState::Off);
        assert_eq!(LoadState::from_code(1), LoadState::On);
        assert_eq!(LoadState::Off.code(), 0);
        assert_eq!(LoadState::On.code(), 1);
        assert_eq!(LoadState::from_code(7), LoadState::Unknown(7));
        assert_eq!(LoadState::Unknown(7).code(), 7);
        assert_eq!(LoadState::Unknown(255).code(), 255);
        assert_eq!(LoadState::On.label(), "On");
        assert_eq!(LoadState::Unknown(2).label(), "Unknown");
    }

    #[test]
    fn connection_health_is_bounded() {
        assert_eq!(ConnectionHealth::Up.label(), "Up");
        assert_eq!(ConnectionHealth::Down.label(), "Down");
        assert_eq!(ConnectionHealth::Unknown.label(), "Unknown");
        let h = ConnectionHealth::Up;
        let h2 = h; // Copy
        assert_eq!(h, h2);
    }

    #[test]
    fn states_are_copy_eq_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ChargerState::Bulk);
        set.insert(ChargerState::Unknown(7));
        assert!(set.contains(&ChargerState::Bulk));
        assert!(set.contains(&ChargerState::Unknown(7)));
        assert!(!set.contains(&ChargerState::Unknown(8)));
        assert_eq!(ChargerState::Bulk, ChargerState::Bulk);
        assert_ne!(ChargerState::Bulk, ChargerState::Float);
    }
}
