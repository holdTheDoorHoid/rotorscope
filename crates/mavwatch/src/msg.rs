//! Message IDs and field offsets (wire order, from common.xml; see
//! [`crate::frame`] for how they were derived).

pub const HEARTBEAT: u32 = 0;
pub const SYS_STATUS: u32 = 1;
pub const SET_MODE: u32 = 11;
pub const PARAM_VALUE: u32 = 22;
pub const PARAM_SET: u32 = 23;
pub const GPS_RAW_INT: u32 = 24;
pub const GLOBAL_POSITION_INT: u32 = 33;
pub const MISSION_COUNT: u32 = 44;
pub const RC_CHANNELS: u32 = 65;
pub const MANUAL_CONTROL: u32 = 69;
pub const RC_CHANNELS_OVERRIDE: u32 = 70;
pub const MISSION_ITEM_INT: u32 = 73;
pub const COMMAND_INT: u32 = 75;
pub const COMMAND_LONG: u32 = 76;
pub const RADIO_STATUS: u32 = 109;
pub const FILE_TRANSFER_PROTOCOL: u32 = 110;
pub const SERIAL_CONTROL: u32 = 126;
pub const HOME_POSITION: u32 = 242;
pub const STATUSTEXT: u32 = 253;
pub const SETUP_SIGNING: u32 = 256;
pub const CAMERA_INFORMATION: u32 = 259;
pub const VIDEO_STREAM_INFORMATION: u32 = 269;
pub const COMPONENT_INFORMATION: u32 = 395;
pub const COMPONENT_METADATA: u32 = 397;

/// PARAM_VALUE: param_value f32 @0, param_count u16 @4, param_index u16 @6,
/// param_id char[16] @8, param_type u8 @24.
pub mod param_value {
    pub const VALUE: usize = 0;
    pub const COUNT: usize = 4;
    pub const INDEX: usize = 6;
    pub const ID: usize = 8;
    pub const TYPE: usize = 24;
}

/// PARAM_SET: param_value f32 @0, target_system @4, target_component @5,
/// param_id char[16] @6, param_type @22.
pub mod param_set {
    pub const VALUE: usize = 0;
    pub const TARGET_SYSTEM: usize = 4;
    pub const TARGET_COMPONENT: usize = 5;
    pub const ID: usize = 6;
    pub const TYPE: usize = 22;
}

/// `MAV_PARAM_TYPE_REAL32` / `REAL64`.
pub const PARAM_TYPE_REAL32: u8 = 9;
pub const PARAM_TYPE_REAL64: u8 = 10;

/// SERIAL_CONTROL: baudrate u32 @0, timeout u16 @4, device @6, flags @7,
/// count @8, data[70] @9; extensions target_system @79, target_component @80.
pub mod serial_control {
    pub const DEVICE: usize = 6;
    pub const FLAGS: usize = 7;
    pub const COUNT: usize = 8;
    pub const DATA: usize = 9;
    pub const DATA_LEN: usize = 70;
    pub const TARGET_SYSTEM: usize = 79;
    pub const TARGET_COMPONENT: usize = 80;
    /// `SERIAL_CONTROL_DEV_SHELL`: the PX4 NSH shell.
    pub const DEV_SHELL: u8 = 10;
}

/// STATUSTEXT: severity @0, text char[50] @1; extensions id u16 @51,
/// chunk_seq @53.
pub mod statustext {
    pub const TEXT: usize = 1;
    pub const TEXT_LEN: usize = 50;
}

/// CAMERA_INFORMATION: vendor_name u8[32] @30, model_name u8[32] @62,
/// cam_definition_uri char[140] @95.
pub mod camera_information {
    pub const VENDOR: usize = 30;
    pub const MODEL: usize = 62;
    pub const NAME_LEN: usize = 32;
    pub const URI: usize = 95;
    pub const URI_LEN: usize = 140;
}

/// VIDEO_STREAM_INFORMATION: name char[32] @21, uri char[160] @53.
pub mod video_stream_information {
    pub const URI: usize = 53;
    pub const URI_LEN: usize = 160;
}

/// COMPONENT_INFORMATION: general_metadata_uri char[100] @12,
/// peripherals_metadata_uri char[100] @112.
pub mod component_information {
    pub const GENERAL_URI: usize = 12;
    pub const PERIPHERALS_URI: usize = 112;
    pub const URI_LEN: usize = 100;
}

/// COMPONENT_METADATA: uri char[100] @8.
pub mod component_metadata {
    pub const URI: usize = 8;
    pub const URI_LEN: usize = 100;
}

/// COMMAND_LONG target_system @30, target_component @31; COMMAND_INT the
/// same offsets.
pub mod command {
    pub const TARGET_SYSTEM: usize = 30;
    pub const TARGET_COMPONENT: usize = 31;
}

/// SETUP_SIGNING: target_system @8, target_component @9, secret_key @10.
pub mod setup_signing {
    pub const TARGET_SYSTEM: usize = 8;
    pub const TARGET_COMPONENT: usize = 9;
}

/// FILE_TRANSFER_PROTOCOL: target_network @0, target_system @1,
/// target_component @2, payload[251] @3. Inside the payload, the MAVLink
/// FTP header (mavlink.io/en/services/ftp.html): seq_number u16 @0,
/// session @2, opcode @3, size @4, req_opcode @5, burst_complete @6,
/// padding @7, offset u32 @8, data[239] @12.
pub mod ftp {
    pub const TARGET_SYSTEM: usize = 1;
    pub const TARGET_COMPONENT: usize = 2;
    /// Where the FTP header starts in the MAVLink payload.
    pub const BASE: usize = 3;
    pub const OPCODE: usize = BASE + 3;
    pub const SIZE: usize = BASE + 4;
    pub const REQ_OPCODE: usize = BASE + 5;
    pub const OFFSET: usize = BASE + 8;
    pub const DATA: usize = BASE + 12;
    pub const DATA_LEN: usize = 239;

    pub const LIST_DIRECTORY: u8 = 3;
    pub const OPEN_FILE_RO: u8 = 4;
    pub const READ_FILE: u8 = 5;
    pub const CREATE_FILE: u8 = 6;
    pub const WRITE_FILE: u8 = 7;
    pub const REMOVE_FILE: u8 = 8;
    pub const CREATE_DIRECTORY: u8 = 9;
    pub const REMOVE_DIRECTORY: u8 = 10;
    pub const OPEN_FILE_WO: u8 = 11;
    pub const TRUNCATE_FILE: u8 = 12;
    pub const RENAME: u8 = 13;
    pub const CALC_FILE_CRC32: u8 = 14;
    pub const BURST_READ_FILE: u8 = 15;
    pub const ACK: u8 = 128;
    pub const NAK: u8 = 129;

    /// Requests whose data is a path.
    pub const PATH_OPCODES: [u8; 10] = [
        LIST_DIRECTORY,
        OPEN_FILE_RO,
        CREATE_FILE,
        REMOVE_FILE,
        CREATE_DIRECTORY,
        REMOVE_DIRECTORY,
        OPEN_FILE_WO,
        TRUNCATE_FILE,
        RENAME,
        CALC_FILE_CRC32,
    ];
}

/// Messages that change what a vehicle does: what a signing downgrade
/// would carry.
pub const COMMANDS: [u32; 11] = [
    SET_MODE,
    PARAM_SET,
    MISSION_COUNT,
    MANUAL_CONTROL,
    RC_CHANNELS_OVERRIDE,
    MISSION_ITEM_INT,
    COMMAND_INT,
    COMMAND_LONG,
    FILE_TRANSFER_PROTOCOL,
    SERIAL_CONTROL,
    SETUP_SIGNING,
];
