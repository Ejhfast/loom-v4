//! The stable core role slots.
//!
//! The VM and the verifier need the core enum families: `Option`,
//! `Result`, `IoError`, and the three virtual-machine event families.
//! An artifact declares them in its core role table, one class index
//! per role. The compiler fills the table, the linker relocates it,
//! and the verifier proves the shape of every filled slot.
//!
//! Reading a slot therefore reads no name, no hash, and no position.
//! An artifact with no source, for example a foreign `.lma`, carries
//! its own table, so the resolution needs nothing outside the bytes.
//!
//! The file `core/pinned-core-defs.txt` pins the structural hash of
//! every core class. It is a determinism gate now, not a resolution
//! mechanism: a core edit that moves a hash must be deliberate. The
//! pin key is the pair `(qualified key, structural hash)`, because a
//! structural hash covers no name and two arms with equal shapes share
//! one value. `StepEvent.Ran` and `StepEvent.Waiting` are the pinned
//! example.

use crate::Module;
use std::collections::HashMap;
use std::sync::OnceLock;

/// The pinned core definition hashes, one `label hash` pair per line.
const PINNED: &str = include_str!("../../../core/pinned-core-defs.txt");

/// The resolved core class indices of one module. Each entry is the
/// class index of one enum case or parent. `None` marks an absent
/// definition.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CoreLayout {
    /// The core method table of immediate integer values.
    pub int: Option<u32>,
    /// The core method table of immediate Boolean values.
    pub boolean: Option<u32>,
    /// The core method table of immutable String values.
    pub string: Option<u32>,
    /// The sealed abstract parent of immutable text values.
    pub text: Option<u32>,
    /// The core method table of shared Substring values.
    pub substring: Option<u32>,
    /// The core method table of immediate Char values.
    pub char_value: Option<u32>,
    /// The core method table of immutable Bytes values.
    pub bytes: Option<u32>,
    /// The core method table of StringBuilder values.
    pub string_builder: Option<u32>,
    /// The core method table of ByteBuffer values.
    pub byte_buffer: Option<u32>,
    pub option_some: Option<u32>,
    pub option_none: Option<u32>,
    pub result_ok: Option<u32>,
    pub result_err: Option<u32>,
    pub io_error_failed: Option<u32>,
    pub run_done: Option<u32>,
    pub run_fault: Option<u32>,
    pub step_ran: Option<u32>,
    pub step_waiting: Option<u32>,
    pub step_done: Option<u32>,
    pub step_fault: Option<u32>,
    pub drive_asked: Option<u32>,
    pub drive_done: Option<u32>,
    pub drive_fault: Option<u32>,
    pub recv_msg: Option<u32>,
    pub recv_closed: Option<u32>,
    pub choice_first: Option<u32>,
    pub choice_second: Option<u32>,
    pub send_sent: Option<u32>,
    pub send_closed: Option<u32>,
    pub send_fault: Option<u32>,
    pub proc_done: Option<u32>,
    pub proc_fault: Option<u32>,
    pub proc_error_dead: Option<u32>,
    pub proc_error_not_paused: Option<u32>,
    pub proc_error_already_paused: Option<u32>,
    pub proc_error_in_use: Option<u32>,
    /// The enum parent class indices, aligned with the arms above.
    pub option: Option<u32>,
    pub result: Option<u32>,
    pub io_error: Option<u32>,
    pub run_result: Option<u32>,
    pub step_event: Option<u32>,
    pub drive_event: Option<u32>,
    pub recv: Option<u32>,
    pub choice: Option<u32>,
    pub send_result: Option<u32>,
    pub proc_result: Option<u32>,
    pub proc_error: Option<u32>,
    /// The core class `Proc`, the parent of every proc class.
    pub proc_class: Option<u32>,
    pub snapshot_error: Option<u32>,
    pub snapshot_resource_active: Option<u32>,
    pub snapshot_limit_exceeded: Option<u32>,
    pub snapshot_bad_image: Option<u32>,
    pub restore_error: Option<u32>,
    pub restore_limit_exceeded: Option<u32>,
    pub fs_error: Option<u32>,
    pub fs_error_closed: Option<u32>,
    pub fs_error_failed: Option<u32>,
    pub open_options: Option<u32>,
    pub open_read_only: Option<u32>,
    pub open_write_only: Option<u32>,
    pub open_read_write: Option<u32>,
    pub open_create: Option<u32>,
    pub open_create_truncate: Option<u32>,
    pub open_append: Option<u32>,
    pub seek_from: Option<u32>,
    pub seek_start: Option<u32>,
    pub seek_current: Option<u32>,
    pub seek_end: Option<u32>,
    pub pair: Option<u32>,
    pub ip_address: Option<u32>,
    pub ip_v4: Option<u32>,
    pub ip_v6: Option<u32>,
    pub socket_address: Option<u32>,
    pub net_error: Option<u32>,
    pub net_invalid_input: Option<u32>,
    pub net_name_not_found: Option<u32>,
    pub net_unavailable: Option<u32>,
    pub net_permission_denied: Option<u32>,
    pub net_address_in_use: Option<u32>,
    pub net_connection_refused: Option<u32>,
    pub net_connection_reset: Option<u32>,
    pub net_not_connected: Option<u32>,
    pub net_timed_out: Option<u32>,
    pub net_closed: Option<u32>,
    pub net_limit_exceeded: Option<u32>,
    pub net_unsupported: Option<u32>,
    pub net_failed: Option<u32>,
    pub tcp_read: Option<u32>,
    pub tcp_read_data: Option<u32>,
    pub tcp_read_end: Option<u32>,
    pub shutdown: Option<u32>,
    pub shutdown_read: Option<u32>,
    pub shutdown_write: Option<u32>,
    pub shutdown_both: Option<u32>,
    pub tcp_resource: Option<u32>,
    pub tcp_stream: Option<u32>,
    pub tcp_listener: Option<u32>,
    pub tls_error: Option<u32>,
    pub tls_invalid_config: Option<u32>,
    pub tls_handshake: Option<u32>,
    pub tls_certificate: Option<u32>,
    pub tls_protocol: Option<u32>,
    pub tls_network: Option<u32>,
    pub tls_closed: Option<u32>,
    pub tls_limit_exceeded: Option<u32>,
    pub tls_stream: Option<u32>,
    /// The core method table of native list values.
    pub list: Option<u32>,
    /// The core method table of native map values.
    pub map: Option<u32>,
    pub artifact: Option<u32>,
    pub verified_module: Option<u32>,
    pub function_code: Option<u32>,
    pub class_code: Option<u32>,
    pub definition_source: Option<u32>,
    pub source_range: Option<u32>,
    pub code_location: Option<u32>,
    pub slot_spec: Option<u32>,
    pub instance: Option<u32>,
    pub slot: Option<u32>,
    pub function_def: Option<u32>,
    pub class_def: Option<u32>,
    pub function_binding: Option<u32>,
    pub class_binding: Option<u32>,
    pub definition_spec: Option<u32>,
    pub slot_change: Option<u32>,
    pub definition_identity: Option<u32>,
    pub code_error: Option<u32>,
    pub link_env: Option<u32>,
    pub compile_env: Option<u32>,
    pub compile_options: Option<u32>,
    pub compile_errors: Option<u32>,
    pub dyn_value: Option<u32>,
    pub syntax_tree: Option<u32>,
    pub syntax_element: Option<u32>,
    pub syntax_node: Option<u32>,
    pub syntax_token: Option<u32>,
    pub syntax_trivia: Option<u32>,
    pub syntax_builder: Option<u32>,
    pub parse_status: Option<u32>,
    pub parse_complete: Option<u32>,
    pub parse_incomplete: Option<u32>,
    pub parse_invalid: Option<u32>,
    pub syntax_diagnostic: Option<u32>,
    pub syntax_parse: Option<u32>,
}

/// The labels of the pinned core definitions, in pin-file order.
pub const PINNED_LABELS: [&str; 143] = [
    "Option",
    "Option.Some",
    "Option.None",
    "Result",
    "Result.Ok",
    "Result.Err",
    "IoError",
    "IoError.Failed",
    "RunResult",
    "RunResult.Done",
    "RunResult.Fault",
    "StepEvent",
    "StepEvent.Ran",
    "StepEvent.Waiting",
    "StepEvent.Done",
    "StepEvent.Fault",
    "DriveEvent",
    "DriveEvent.Asked",
    "DriveEvent.Done",
    "DriveEvent.Fault",
    "Recv",
    "Recv.Msg",
    "Recv.Closed",
    "SendResult",
    "SendResult.Sent",
    "SendResult.Closed",
    "SendResult.Fault",
    "ProcResult",
    "ProcResult.Done",
    "ProcResult.Fault",
    "ProcError",
    "ProcError.Dead",
    "ProcError.NotPaused",
    "ProcError.AlreadyPaused",
    "ProcError.InUse",
    "Proc",
    "SnapshotError",
    "SnapshotError.ResourceActive",
    "SnapshotError.SnapshotLimitExceeded",
    "SnapshotError.BadImage",
    "RestoreError",
    "RestoreError.RestoreLimitExceeded",
    "FsError",
    "FsError.Closed",
    "FsError.Failed",
    "OpenOptions",
    "OpenOptions.ReadOnly",
    "OpenOptions.WriteOnly",
    "OpenOptions.ReadWrite",
    "OpenOptions.Create",
    "OpenOptions.CreateTruncate",
    "OpenOptions.Append",
    "SeekFrom",
    "SeekFrom.Start",
    "SeekFrom.Current",
    "SeekFrom.End",
    "Choice",
    "Choice.First",
    "Choice.Second",
    "Int",
    "Bool",
    "String",
    "Bytes",
    "StringBuilder",
    "ByteBuffer",
    "Text",
    "Substring",
    "Char",
    "Pair",
    "IpAddress",
    "IpAddress.V4",
    "IpAddress.V6",
    "SocketAddress",
    "NetError",
    "NetError.InvalidInput",
    "NetError.NameNotFound",
    "NetError.Unavailable",
    "NetError.PermissionDenied",
    "NetError.AddressInUse",
    "NetError.ConnectionRefused",
    "NetError.ConnectionReset",
    "NetError.NotConnected",
    "NetError.TimedOut",
    "NetError.Closed",
    "NetError.LimitExceeded",
    "NetError.Unsupported",
    "NetError.Failed",
    "TcpRead",
    "TcpRead.Data",
    "TcpRead.End",
    "Shutdown",
    "Shutdown.Read",
    "Shutdown.Write",
    "Shutdown.Both",
    "TcpResource",
    "TcpStream",
    "TcpListener",
    "TlsError",
    "TlsError.InvalidConfig",
    "TlsError.Handshake",
    "TlsError.Certificate",
    "TlsError.Protocol",
    "TlsError.Network",
    "TlsError.Closed",
    "TlsError.LimitExceeded",
    "TlsStream",
    "List",
    "Map",
    "Artifact",
    "VerifiedModule",
    "SlotSpec",
    "Instance",
    "Slot",
    "FunctionDef",
    "CodeError",
    "LinkEnv",
    "CompileEnv",
    "CompileOptions",
    "CompileErrors",
    "SyntaxTree",
    "SyntaxElement",
    "SyntaxNode",
    "SyntaxToken",
    "SyntaxTrivia",
    "SyntaxBuilder",
    "ParseStatus",
    "ParseStatus.ParseComplete",
    "ParseStatus.ParseIncomplete",
    "ParseStatus.ParseInvalid",
    "SyntaxDiagnostic",
    "SyntaxParse",
    "DynValue",
    "ClassDef",
    "FunctionCode",
    "ClassCode",
    "DefinitionSource",
    "SourceRange",
    "CodeLocation",
    "FunctionBinding",
    "ClassBinding",
    "DefinitionSpec",
    "SlotChange",
    "DefinitionIdentity",
];

/// The core role of immediate integer values.
pub const ROLE_INT: usize = 59;

/// The core role of the native `Option` family.
pub const ROLE_OPTION: usize = 0;

/// The core role of the native `Some` arm.
pub const ROLE_OPTION_SOME: usize = 1;

/// The core role of the native `None` arm.
pub const ROLE_OPTION_NONE: usize = 2;

/// The core role of immediate Boolean values.
pub const ROLE_BOOL: usize = 60;

/// The core role of immutable String values.
pub const ROLE_STRING: usize = 61;

/// The core role of immutable Bytes values.
pub const ROLE_BYTES: usize = 62;

/// The core role of StringBuilder values.
pub const ROLE_STRING_BUILDER: usize = 63;

/// The core role of ByteBuffer values.
pub const ROLE_BYTE_BUFFER: usize = 64;

/// The core role of the sealed Text parent.
pub const ROLE_TEXT: usize = 65;

/// The core role of shared Substring values.
pub const ROLE_SUBSTRING: usize = 66;

/// The core role of immediate Unicode scalar values.
pub const ROLE_CHAR: usize = 67;

pub const ROLE_PAIR: usize = 68;
pub const ROLE_IP_ADDRESS: usize = 69;
pub const ROLE_IP_V4: usize = 70;
pub const ROLE_IP_V6: usize = 71;
pub const ROLE_SOCKET_ADDRESS: usize = 72;
pub const ROLE_NET_ERROR: usize = 73;
pub const ROLE_NET_INVALID_INPUT: usize = 74;
pub const ROLE_NET_NAME_NOT_FOUND: usize = 75;
pub const ROLE_NET_UNAVAILABLE: usize = 76;
pub const ROLE_NET_PERMISSION_DENIED: usize = 77;
pub const ROLE_NET_ADDRESS_IN_USE: usize = 78;
pub const ROLE_NET_CONNECTION_REFUSED: usize = 79;
pub const ROLE_NET_CONNECTION_RESET: usize = 80;
pub const ROLE_NET_NOT_CONNECTED: usize = 81;
pub const ROLE_NET_TIMED_OUT: usize = 82;
pub const ROLE_NET_CLOSED: usize = 83;
pub const ROLE_NET_LIMIT_EXCEEDED: usize = 84;
pub const ROLE_NET_UNSUPPORTED: usize = 85;
pub const ROLE_NET_FAILED: usize = 86;
pub const ROLE_TCP_READ: usize = 87;
pub const ROLE_TCP_READ_DATA: usize = 88;
pub const ROLE_TCP_READ_END: usize = 89;
pub const ROLE_SHUTDOWN: usize = 90;
pub const ROLE_SHUTDOWN_READ: usize = 91;
pub const ROLE_SHUTDOWN_WRITE: usize = 92;
pub const ROLE_SHUTDOWN_BOTH: usize = 93;
pub const ROLE_TCP_RESOURCE: usize = 94;
pub const ROLE_TCP_STREAM: usize = 95;
pub const ROLE_TCP_LISTENER: usize = 96;
pub const ROLE_TLS_ERROR: usize = 97;
pub const ROLE_TLS_INVALID_CONFIG: usize = 98;
pub const ROLE_TLS_HANDSHAKE: usize = 99;
pub const ROLE_TLS_CERTIFICATE: usize = 100;
pub const ROLE_TLS_PROTOCOL: usize = 101;
pub const ROLE_TLS_NETWORK: usize = 102;
pub const ROLE_TLS_CLOSED: usize = 103;
pub const ROLE_TLS_LIMIT_EXCEEDED: usize = 104;
pub const ROLE_TLS_STREAM: usize = 105;
pub const ROLE_LIST: usize = 106;
pub const ROLE_MAP: usize = 107;
pub const ROLE_ARTIFACT: usize = 108;
pub const ROLE_VERIFIED_MODULE: usize = 109;
pub const ROLE_SLOT_SPEC: usize = 110;
pub const ROLE_INSTANCE: usize = 111;
pub const ROLE_SLOT: usize = 112;
pub const ROLE_FUNCTION_DEF: usize = 113;
pub const ROLE_CODE_ERROR: usize = 114;
pub const ROLE_LINK_ENV: usize = 115;
pub const ROLE_COMPILE_ENV: usize = 116;
pub const ROLE_COMPILE_OPTIONS: usize = 117;
pub const ROLE_COMPILE_ERRORS: usize = 118;
pub const ROLE_SYNTAX_TREE: usize = 119;
pub const ROLE_SYNTAX_ELEMENT: usize = 120;
pub const ROLE_SYNTAX_NODE: usize = 121;
pub const ROLE_SYNTAX_TOKEN: usize = 122;
pub const ROLE_SYNTAX_TRIVIA: usize = 123;
pub const ROLE_SYNTAX_BUILDER: usize = 124;
pub const ROLE_PARSE_STATUS: usize = 125;
pub const ROLE_PARSE_COMPLETE: usize = 126;
pub const ROLE_PARSE_INCOMPLETE: usize = 127;
pub const ROLE_PARSE_INVALID: usize = 128;
pub const ROLE_SYNTAX_DIAGNOSTIC: usize = 129;
pub const ROLE_SYNTAX_PARSE: usize = 130;
pub const ROLE_DYN_VALUE: usize = 131;
pub const ROLE_CLASS_DEF: usize = 132;
pub const ROLE_FUNCTION_CODE: usize = 133;
pub const ROLE_CLASS_CODE: usize = 134;
pub const ROLE_DEFINITION_SOURCE: usize = 135;
pub const ROLE_SOURCE_RANGE: usize = 136;
pub const ROLE_CODE_LOCATION: usize = 137;
pub const ROLE_FUNCTION_BINDING: usize = 138;
pub const ROLE_CLASS_BINDING: usize = 139;
pub const ROLE_DEFINITION_SPEC: usize = 140;
pub const ROLE_SLOT_CHANGE: usize = 141;
pub const ROLE_DEFINITION_IDENTITY: usize = 142;

fn parse_hex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in text.as_bytes().chunks(2).enumerate() {
        let hex = std::str::from_utf8(chunk).ok()?;
        out[i] = u8::from_str_radix(hex, 16).ok()?;
    }
    Some(out)
}

/// The qualified key of one pinned core label.
pub fn pinned_key(label: &str) -> String {
    crate::qualified_key(crate::CORE_MODULE, label)
}

/// The pinned table: `(qualified key, structural hash)` to slot label.
fn pinned_map() -> &'static HashMap<(String, [u8; 32]), &'static str> {
    static MAP: OnceLock<HashMap<(String, [u8; 32]), &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        for line in PINNED.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (label, hex) = line
                .split_once(' ')
                .expect("a pinned core line is `label hash`");
            let hash = parse_hex(hex.trim()).expect("a pinned core hash is 64 hex digits");
            let label = PINNED_LABELS
                .iter()
                .copied()
                .find(|l| *l == label)
                .expect("a pinned core label is known");
            map.insert((pinned_key(label), hash), label);
        }
        assert_eq!(
            map.len(),
            PINNED_LABELS.len(),
            "the pinned core table must cover every label once"
        );
        map
    })
}

fn slot_mut<'a>(layout: &'a mut CoreLayout, label: &str) -> &'a mut Option<u32> {
    match label {
        "Int" => &mut layout.int,
        "Bool" => &mut layout.boolean,
        "String" => &mut layout.string,
        "Text" => &mut layout.text,
        "Substring" => &mut layout.substring,
        "Char" => &mut layout.char_value,
        "Bytes" => &mut layout.bytes,
        "StringBuilder" => &mut layout.string_builder,
        "ByteBuffer" => &mut layout.byte_buffer,
        "Option" => &mut layout.option,
        "Option.Some" => &mut layout.option_some,
        "Option.None" => &mut layout.option_none,
        "Result" => &mut layout.result,
        "Result.Ok" => &mut layout.result_ok,
        "Result.Err" => &mut layout.result_err,
        "IoError" => &mut layout.io_error,
        "IoError.Failed" => &mut layout.io_error_failed,
        "RunResult" => &mut layout.run_result,
        "RunResult.Done" => &mut layout.run_done,
        "RunResult.Fault" => &mut layout.run_fault,
        "StepEvent" => &mut layout.step_event,
        "StepEvent.Ran" => &mut layout.step_ran,
        "StepEvent.Waiting" => &mut layout.step_waiting,
        "StepEvent.Done" => &mut layout.step_done,
        "StepEvent.Fault" => &mut layout.step_fault,
        "DriveEvent" => &mut layout.drive_event,
        "DriveEvent.Asked" => &mut layout.drive_asked,
        "DriveEvent.Done" => &mut layout.drive_done,
        "DriveEvent.Fault" => &mut layout.drive_fault,
        "Recv" => &mut layout.recv,
        "Recv.Msg" => &mut layout.recv_msg,
        "Recv.Closed" => &mut layout.recv_closed,
        "Choice" => &mut layout.choice,
        "Choice.First" => &mut layout.choice_first,
        "Choice.Second" => &mut layout.choice_second,
        "SendResult" => &mut layout.send_result,
        "SendResult.Sent" => &mut layout.send_sent,
        "SendResult.Closed" => &mut layout.send_closed,
        "SendResult.Fault" => &mut layout.send_fault,
        "ProcResult" => &mut layout.proc_result,
        "ProcResult.Done" => &mut layout.proc_done,
        "ProcResult.Fault" => &mut layout.proc_fault,
        "ProcError" => &mut layout.proc_error,
        "ProcError.Dead" => &mut layout.proc_error_dead,
        "ProcError.NotPaused" => &mut layout.proc_error_not_paused,
        "ProcError.AlreadyPaused" => &mut layout.proc_error_already_paused,
        "ProcError.InUse" => &mut layout.proc_error_in_use,
        "Proc" => &mut layout.proc_class,
        "SnapshotError" => &mut layout.snapshot_error,
        "SnapshotError.ResourceActive" => &mut layout.snapshot_resource_active,
        "SnapshotError.SnapshotLimitExceeded" => &mut layout.snapshot_limit_exceeded,
        "SnapshotError.BadImage" => &mut layout.snapshot_bad_image,
        "RestoreError" => &mut layout.restore_error,
        "RestoreError.RestoreLimitExceeded" => &mut layout.restore_limit_exceeded,
        "FsError" => &mut layout.fs_error,
        "FsError.Closed" => &mut layout.fs_error_closed,
        "FsError.Failed" => &mut layout.fs_error_failed,
        "OpenOptions" => &mut layout.open_options,
        "OpenOptions.ReadOnly" => &mut layout.open_read_only,
        "OpenOptions.WriteOnly" => &mut layout.open_write_only,
        "OpenOptions.ReadWrite" => &mut layout.open_read_write,
        "OpenOptions.Create" => &mut layout.open_create,
        "OpenOptions.CreateTruncate" => &mut layout.open_create_truncate,
        "OpenOptions.Append" => &mut layout.open_append,
        "SeekFrom" => &mut layout.seek_from,
        "SeekFrom.Start" => &mut layout.seek_start,
        "SeekFrom.Current" => &mut layout.seek_current,
        "SeekFrom.End" => &mut layout.seek_end,
        "Pair" => &mut layout.pair,
        "IpAddress" => &mut layout.ip_address,
        "IpAddress.V4" => &mut layout.ip_v4,
        "IpAddress.V6" => &mut layout.ip_v6,
        "SocketAddress" => &mut layout.socket_address,
        "NetError" => &mut layout.net_error,
        "NetError.InvalidInput" => &mut layout.net_invalid_input,
        "NetError.NameNotFound" => &mut layout.net_name_not_found,
        "NetError.Unavailable" => &mut layout.net_unavailable,
        "NetError.PermissionDenied" => &mut layout.net_permission_denied,
        "NetError.AddressInUse" => &mut layout.net_address_in_use,
        "NetError.ConnectionRefused" => &mut layout.net_connection_refused,
        "NetError.ConnectionReset" => &mut layout.net_connection_reset,
        "NetError.NotConnected" => &mut layout.net_not_connected,
        "NetError.TimedOut" => &mut layout.net_timed_out,
        "NetError.Closed" => &mut layout.net_closed,
        "NetError.LimitExceeded" => &mut layout.net_limit_exceeded,
        "NetError.Unsupported" => &mut layout.net_unsupported,
        "NetError.Failed" => &mut layout.net_failed,
        "TcpRead" => &mut layout.tcp_read,
        "TcpRead.Data" => &mut layout.tcp_read_data,
        "TcpRead.End" => &mut layout.tcp_read_end,
        "Shutdown" => &mut layout.shutdown,
        "Shutdown.Read" => &mut layout.shutdown_read,
        "Shutdown.Write" => &mut layout.shutdown_write,
        "Shutdown.Both" => &mut layout.shutdown_both,
        "TcpResource" => &mut layout.tcp_resource,
        "TcpStream" => &mut layout.tcp_stream,
        "TcpListener" => &mut layout.tcp_listener,
        "TlsError" => &mut layout.tls_error,
        "TlsError.InvalidConfig" => &mut layout.tls_invalid_config,
        "TlsError.Handshake" => &mut layout.tls_handshake,
        "TlsError.Certificate" => &mut layout.tls_certificate,
        "TlsError.Protocol" => &mut layout.tls_protocol,
        "TlsError.Network" => &mut layout.tls_network,
        "TlsError.Closed" => &mut layout.tls_closed,
        "TlsError.LimitExceeded" => &mut layout.tls_limit_exceeded,
        "TlsStream" => &mut layout.tls_stream,
        "List" => &mut layout.list,
        "Map" => &mut layout.map,
        "Artifact" => &mut layout.artifact,
        "VerifiedModule" => &mut layout.verified_module,
        "FunctionCode" => &mut layout.function_code,
        "ClassCode" => &mut layout.class_code,
        "DefinitionSource" => &mut layout.definition_source,
        "SourceRange" => &mut layout.source_range,
        "CodeLocation" => &mut layout.code_location,
        "SlotSpec" => &mut layout.slot_spec,
        "Instance" => &mut layout.instance,
        "Slot" => &mut layout.slot,
        "FunctionDef" => &mut layout.function_def,
        "ClassDef" => &mut layout.class_def,
        "FunctionBinding" => &mut layout.function_binding,
        "ClassBinding" => &mut layout.class_binding,
        "DefinitionSpec" => &mut layout.definition_spec,
        "SlotChange" => &mut layout.slot_change,
        "DefinitionIdentity" => &mut layout.definition_identity,
        "CodeError" => &mut layout.code_error,
        "LinkEnv" => &mut layout.link_env,
        "CompileEnv" => &mut layout.compile_env,
        "CompileOptions" => &mut layout.compile_options,
        "CompileErrors" => &mut layout.compile_errors,
        "DynValue" => &mut layout.dyn_value,
        "SyntaxTree" => &mut layout.syntax_tree,
        "SyntaxElement" => &mut layout.syntax_element,
        "SyntaxNode" => &mut layout.syntax_node,
        "SyntaxToken" => &mut layout.syntax_token,
        "SyntaxTrivia" => &mut layout.syntax_trivia,
        "SyntaxBuilder" => &mut layout.syntax_builder,
        "ParseStatus" => &mut layout.parse_status,
        "ParseStatus.ParseComplete" => &mut layout.parse_complete,
        "ParseStatus.ParseIncomplete" => &mut layout.parse_incomplete,
        "ParseStatus.ParseInvalid" => &mut layout.parse_invalid,
        "SyntaxDiagnostic" => &mut layout.syntax_diagnostic,
        "SyntaxParse" => &mut layout.syntax_parse,
        _ => unreachable!("only known labels enter the map"),
    }
}

fn set_slot(layout: &mut CoreLayout, label: &str, idx: u32) {
    *slot_mut(layout, label) = Some(idx);
}

fn slot_of(layout: &CoreLayout, label: &str) -> Option<u32> {
    // The read borrows nothing mutable; the clone keeps one label
    // table instead of two.
    let mut copy = *layout;
    *slot_mut(&mut copy, label)
}

/// The core role index of one pinned label, or `None` when the label
/// is unknown. The order is `PINNED_LABELS`.
pub fn role_index(label: &str) -> Option<usize> {
    PINNED_LABELS.iter().position(|l| *l == label)
}

/// Read the declared core layout of one module.
///
/// The artifact carries the table, the compiler filled it, and the
/// verifier proves the shape of every filled slot. This function reads
/// slots only: no name, no hash, and no position takes part.
pub fn declared_layout(module: &Module) -> CoreLayout {
    let mut layout = CoreLayout::default();
    for (role, slot) in module.core_roles.iter().enumerate() {
        if *slot == crate::NO_ROLE {
            continue;
        }
        set_slot(&mut layout, PINNED_LABELS[role], *slot);
    }
    layout
}

/// The core role table that matches one layout, for a module builder.
pub fn roles_of(layout: &CoreLayout) -> [u32; crate::CORE_ROLE_COUNT] {
    let mut roles = [crate::NO_ROLE; crate::CORE_ROLE_COUNT];
    for (role, label) in PINNED_LABELS.iter().enumerate() {
        if let Some(idx) = slot_of(layout, label) {
            roles[role] = idx;
        }
    }
    roles
}

/// The pinned structural hash of one label, for the determinism gate.
pub fn pinned_hash(label: &str) -> Option<[u8; 32]> {
    pinned_map()
        .iter()
        .find(|((key, _), found)| *found == &label && *key == pinned_key(label))
        .map(|((_, hash), _)| *hash)
}
