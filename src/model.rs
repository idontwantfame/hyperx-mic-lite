use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub(crate) struct DeviceInfo {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) is_default: bool,
}

#[derive(Serialize)]
pub(crate) struct MicStatus {
    pub(crate) device: DeviceInfo,
    pub(crate) volume: u8,
    pub(crate) muted: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PolarPattern {
    Stereo,
    Omni,
    Cardioid,
    Bidirectional,
    Unknown(u8),
}

impl PolarPattern {
    pub(crate) fn from_report(value: u8) -> Self {
        match value {
            0 => Self::Stereo,
            1 => Self::Omni,
            2 => Self::Cardioid,
            3 => Self::Bidirectional,
            other => Self::Unknown(other),
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Stereo => "Stereo",
            Self::Omni => "Omni",
            Self::Cardioid => "Cardioid",
            Self::Bidirectional => "Bidirectional",
            Self::Unknown(_) => "Unknown",
        }
    }

    pub(crate) fn from_config(value: &str) -> Self {
        match value {
            "stereo" => Self::Stereo,
            "omni" => Self::Omni,
            "cardioid" => Self::Cardioid,
            "bidirectional" => Self::Bidirectional,
            _ => Self::Unknown(255),
        }
    }

    pub(crate) fn as_config(self) -> &'static str {
        match self {
            Self::Stereo => "stereo",
            Self::Omni => "omni",
            Self::Cardioid => "cardioid",
            Self::Bidirectional => "bidirectional",
            Self::Unknown(_) => "unknown",
        }
    }
}

pub(crate) enum HidEvent {
    Mute(bool),
    Pattern(PolarPattern),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Audio,
    Lights,
}

impl Tab {
    pub(crate) fn from_config(value: &str) -> Self {
        match value {
            "lights" => Self::Lights,
            _ => Self::Audio,
        }
    }

    pub(crate) fn as_config(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Lights => "lights",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    Wave,
    Solid,
    Cycle,
    Pulse,
    Blink,
    Lightning,
    VuMeter,
}

impl Effect {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Wave => "Wave",
            Self::Solid => "Solid",
            Self::Cycle => "Cycle",
            Self::Pulse => "Pulse",
            Self::Blink => "Blink",
            Self::Lightning => "Lightning",
            Self::VuMeter => "VU Meter",
        }
    }

    pub(crate) fn from_config(value: &str) -> Self {
        match value {
            "solid" => Self::Solid,
            "cycle" => Self::Cycle,
            "pulse" => Self::Pulse,
            "blink" => Self::Blink,
            "lightning" => Self::Lightning,
            "vu_meter" => Self::VuMeter,
            _ => Self::Wave,
        }
    }

    pub(crate) fn as_config(self) -> &'static str {
        match self {
            Self::Wave => "wave",
            Self::Solid => "solid",
            Self::Cycle => "cycle",
            Self::Pulse => "pulse",
            Self::Blink => "blink",
            Self::Lightning => "lightning",
            Self::VuMeter => "vu_meter",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LightTarget {
    All,
    Top,
    Bottom,
}

impl LightTarget {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Top => "Top",
            Self::Bottom => "Bottom",
        }
    }

    pub(crate) fn from_config(value: &str) -> Self {
        match value {
            "top" => Self::Top,
            "bottom" => Self::Bottom,
            _ => Self::All,
        }
    }

    pub(crate) fn as_config(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
}

#[derive(Serialize)]
pub(crate) struct LightingDevice {
    pub(crate) vendor_id: u16,
    pub(crate) product_id: u16,
    pub(crate) interface_number: i32,
    pub(crate) usage_page: u16,
    pub(crate) usage: u16,
    pub(crate) manufacturer: String,
    pub(crate) product: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ServiceHealth {
    pub(crate) schema_version: u32,
    pub(crate) service_name: String,
    pub(crate) state: String,
    pub(crate) pid: u32,
    pub(crate) updated_at: String,
    pub(crate) heartbeat_count: u64,
    pub(crate) restore_on_startup: bool,
    pub(crate) last_restore: Option<String>,
    pub(crate) last_error: Option<String>,
}
