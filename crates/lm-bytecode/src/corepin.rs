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
    /// The core method table of immediate floating-point values.
    pub float: Option<u32>,
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
    /// The core method table of native file handles.
    pub file_handle: Option<u32>,
    /// The core method table of StringBuilder values.
    pub string_builder: Option<u32>,
    /// The core method table of ByteBuffer values.
    pub byte_buffer: Option<u32>,
    /// The core method table of unit values.
    pub unit: Option<u32>,
    /// Native tuple method tables indexed by arity.
    pub tuples: [Option<u32>; 17],
    pub option_some: Option<u32>,
    pub option_none: Option<u32>,
    pub result_ok: Option<u32>,
    pub result_err: Option<u32>,
    pub io_error_broken_pipe: Option<u32>,
    pub io_error_invalid_input: Option<u32>,
    pub io_error_limit_exceeded: Option<u32>,
    pub io_error_unsupported: Option<u32>,
    pub io_error_failed: Option<u32>,
    pub env_error_invalid_name: Option<u32>,
    pub env_error_invalid_encoding: Option<u32>,
    pub env_error_permission_denied: Option<u32>,
    pub env_error_failed: Option<u32>,
    pub entropy_error_invalid_input: Option<u32>,
    pub entropy_error_limit_exceeded: Option<u32>,
    pub entropy_error_unavailable: Option<u32>,
    pub entropy_error_failed: Option<u32>,
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
    pub proc_error_dead: Option<u32>,
    pub proc_error_not_paused: Option<u32>,
    pub proc_error_already_paused: Option<u32>,
    pub proc_error_in_use: Option<u32>,
    /// The enum parent class indices, aligned with the arms above.
    pub option: Option<u32>,
    pub result: Option<u32>,
    pub io_error: Option<u32>,
    pub env_error: Option<u32>,
    pub entropy_error: Option<u32>,
    pub step_event: Option<u32>,
    pub drive_event: Option<u32>,
    pub recv: Option<u32>,
    pub choice: Option<u32>,
    pub send_result: Option<u32>,
    pub proc_error: Option<u32>,
    /// The core class `Proc`, the parent of every proc class.
    pub proc_class: Option<u32>,
    pub snapshot_error: Option<u32>,
    pub snapshot_resource_active: Option<u32>,
    pub snapshot_limit_exceeded: Option<u32>,
    pub snapshot_bad_image: Option<u32>,
    pub restore_error: Option<u32>,
    pub restore_limit_exceeded: Option<u32>,
    pub branch_error: Option<u32>,
    pub branch_resource_active: Option<u32>,
    pub branch_limit_exceeded: Option<u32>,
    pub fs_error: Option<u32>,
    pub fs_error_closed: Option<u32>,
    pub fs_error_invalid_input: Option<u32>,
    pub fs_error_invalid_encoding: Option<u32>,
    pub fs_error_limit_exceeded: Option<u32>,
    pub fs_error_not_found: Option<u32>,
    pub fs_error_already_exists: Option<u32>,
    pub fs_error_permission_denied: Option<u32>,
    pub fs_error_not_directory: Option<u32>,
    pub fs_error_is_directory: Option<u32>,
    pub fs_error_directory_not_empty: Option<u32>,
    pub fs_error_cross_device: Option<u32>,
    pub fs_error_unsupported: Option<u32>,
    pub fs_error_failed: Option<u32>,
    pub open_options: Option<u32>,
    pub open_read_only: Option<u32>,
    pub open_write_only: Option<u32>,
    pub open_read_write: Option<u32>,
    pub open_create: Option<u32>,
    pub open_create_truncate: Option<u32>,
    pub open_create_new: Option<u32>,
    pub open_append: Option<u32>,
    pub seek_from: Option<u32>,
    pub seek_start: Option<u32>,
    pub seek_current: Option<u32>,
    pub seek_end: Option<u32>,
    pub file_kind: Option<u32>,
    pub file_kind_file: Option<u32>,
    pub file_kind_directory: Option<u32>,
    pub file_kind_symlink: Option<u32>,
    pub file_kind_other: Option<u32>,
    pub file_info: Option<u32>,
    pub dir_entry: Option<u32>,
    pub rename_mode: Option<u32>,
    pub rename_no_replace: Option<u32>,
    pub rename_replace: Option<u32>,
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
    pub std_stream: Option<u32>,
    pub std_stream_input: Option<u32>,
    pub std_stream_output: Option<u32>,
    pub std_stream_error: Option<u32>,
    pub tty_size: Option<u32>,
    pub tty_error: Option<u32>,
    pub tty_error_closed: Option<u32>,
    pub tty_error_not_terminal: Option<u32>,
    pub tty_error_busy: Option<u32>,
    pub tty_error_permission_denied: Option<u32>,
    pub tty_error_unsupported: Option<u32>,
    pub tty_error_failed: Option<u32>,
    pub raw_mode: Option<u32>,
    pub signal_kind: Option<u32>,
    pub signal_interrupt: Option<u32>,
    pub signal_terminate: Option<u32>,
    pub signal_error: Option<u32>,
    pub signal_error_closed: Option<u32>,
    pub signal_error_invalid_input: Option<u32>,
    pub signal_error_busy: Option<u32>,
    pub signal_error_unsupported: Option<u32>,
    pub signal_error_limit_exceeded: Option<u32>,
    pub signal_error_failed: Option<u32>,
    pub signal_stream: Option<u32>,
    pub pipe_error: Option<u32>,
    pub pipe_error_closed: Option<u32>,
    pub pipe_error_broken_pipe: Option<u32>,
    pub pipe_error_invalid_input: Option<u32>,
    pub pipe_error_limit_exceeded: Option<u32>,
    pub pipe_error_unsupported: Option<u32>,
    pub pipe_error_failed: Option<u32>,
    pub pipe_end: Option<u32>,
    pub pipe_reader: Option<u32>,
    pub pipe_writer: Option<u32>,
    pub child_input: Option<u32>,
    pub child_input_inherit: Option<u32>,
    pub child_input_null: Option<u32>,
    pub child_input_pipe: Option<u32>,
    pub child_output: Option<u32>,
    pub child_output_inherit: Option<u32>,
    pub child_output_null: Option<u32>,
    pub child_output_pipe: Option<u32>,
    pub child_env: Option<u32>,
    pub child_env_inherit: Option<u32>,
    pub child_env_exact: Option<u32>,
    pub child_env_overlay: Option<u32>,
    pub exec_spec: Option<u32>,
    pub child_status: Option<u32>,
    pub child_status_exited: Option<u32>,
    pub child_status_terminated: Option<u32>,
    pub exec_error: Option<u32>,
    pub exec_error_closed: Option<u32>,
    pub exec_error_invalid_input: Option<u32>,
    pub exec_error_limit_exceeded: Option<u32>,
    pub exec_error_not_found: Option<u32>,
    pub exec_error_permission_denied: Option<u32>,
    pub exec_error_unsupported: Option<u32>,
    pub exec_error_failed: Option<u32>,
    pub child: Option<u32>,
    pub udp_datagram: Option<u32>,
    pub udp_socket: Option<u32>,
}

/// The labels of the pinned core definitions, in pin-file order.
pub const PINNED_LABELS: [&str; 254] = [
    "Option",
    "Option.Some",
    "Option.None",
    "Result",
    "Result.Ok",
    "Result.Err",
    "IoError",
    "IoError.Failed",
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
    "Tuple2",
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
    "IoError.BrokenPipe",
    "IoError.InvalidInput",
    "IoError.LimitExceeded",
    "EnvError",
    "EnvError.InvalidName",
    "EnvError.InvalidEncoding",
    "EnvError.PermissionDenied",
    "EnvError.Failed",
    "EntropyError",
    "EntropyError.InvalidInput",
    "EntropyError.LimitExceeded",
    "EntropyError.Unavailable",
    "EntropyError.Failed",
    "Tuple3",
    "Tuple4",
    "Tuple5",
    "Tuple6",
    "Tuple7",
    "Tuple8",
    "Tuple9",
    "Tuple10",
    "Tuple11",
    "Tuple12",
    "Tuple13",
    "Tuple14",
    "Tuple15",
    "Tuple16",
    "Unit",
    "Float",
    "StdStream",
    "StdStream.Input",
    "StdStream.Output",
    "StdStream.Error",
    "TtySize",
    "TtyError",
    "TtyError.Closed",
    "TtyError.NotTerminal",
    "TtyError.Busy",
    "TtyError.PermissionDenied",
    "TtyError.Unsupported",
    "TtyError.Failed",
    "RawMode",
    "SignalKind",
    "SignalKind.Interrupt",
    "SignalKind.Terminate",
    "SignalError",
    "SignalError.Closed",
    "SignalError.InvalidInput",
    "SignalError.Busy",
    "SignalError.Unsupported",
    "SignalError.LimitExceeded",
    "SignalError.Failed",
    "SignalStream",
    "IoError.Unsupported",
    "FsError.InvalidInput",
    "FsError.InvalidEncoding",
    "FsError.LimitExceeded",
    "FsError.NotFound",
    "FsError.AlreadyExists",
    "FsError.PermissionDenied",
    "FsError.NotDirectory",
    "FsError.IsDirectory",
    "FsError.DirectoryNotEmpty",
    "FsError.CrossDevice",
    "FsError.Unsupported",
    "OpenOptions.CreateNew",
    "FileKind",
    "FileKind.File",
    "FileKind.Directory",
    "FileKind.Symlink",
    "FileKind.Other",
    "FileInfo",
    "DirEntry",
    "RenameMode",
    "RenameMode.NoReplace",
    "RenameMode.Replace",
    "PipeError",
    "PipeError.Closed",
    "PipeError.BrokenPipe",
    "PipeError.InvalidInput",
    "PipeError.LimitExceeded",
    "PipeError.Failed",
    "PipeEnd",
    "PipeReader",
    "PipeWriter",
    "ChildInput",
    "ChildInput.Inherit",
    "ChildInput.Null",
    "ChildInput.Pipe",
    "ChildOutput",
    "ChildOutput.Inherit",
    "ChildOutput.Null",
    "ChildOutput.Pipe",
    "ChildEnv",
    "ChildEnv.Inherit",
    "ChildEnv.Exact",
    "ExecSpec",
    "ChildStatus",
    "ChildStatus.Exited",
    "ChildStatus.Terminated",
    "ExecError",
    "ExecError.Closed",
    "ExecError.InvalidInput",
    "ExecError.LimitExceeded",
    "ExecError.NotFound",
    "ExecError.PermissionDenied",
    "ExecError.Unsupported",
    "ExecError.Failed",
    "Child",
    "PipeError.Unsupported",
    "UdpDatagram",
    "UdpSocket",
    "ChildEnv.Overlay",
    "FileHandle",
    "BranchError",
    "BranchError.ResourceActive",
    "BranchError.BranchLimitExceeded",
];

/// The core role of immediate integer values.
pub const ROLE_INT: usize = 53;

/// The core role of the native `Option` family.
pub const ROLE_OPTION: usize = 0;

/// The core role of the native `Some` arm.
pub const ROLE_OPTION_SOME: usize = 1;

/// The core role of the native `None` arm.
pub const ROLE_OPTION_NONE: usize = 2;

/// The core role of immediate Boolean values.
pub const ROLE_BOOL: usize = 54;

/// The core role of immutable String values.
pub const ROLE_STRING: usize = 55;

/// The core role of immutable Bytes values.
pub const ROLE_BYTES: usize = 56;

/// The core role of StringBuilder values.
pub const ROLE_STRING_BUILDER: usize = 57;

/// The core role of ByteBuffer values.
pub const ROLE_BYTE_BUFFER: usize = 58;

/// The core role of the sealed Text parent.
pub const ROLE_TEXT: usize = 59;

/// The core role of shared Substring values.
pub const ROLE_SUBSTRING: usize = 60;

/// The core role of immediate Unicode scalar values.
pub const ROLE_CHAR: usize = 61;

pub const ROLE_TUPLE2: usize = 62;
pub const ROLE_IP_ADDRESS: usize = 63;
pub const ROLE_IP_V4: usize = 64;
pub const ROLE_IP_V6: usize = 65;
pub const ROLE_SOCKET_ADDRESS: usize = 66;
pub const ROLE_NET_ERROR: usize = 67;
pub const ROLE_NET_INVALID_INPUT: usize = 68;
pub const ROLE_NET_NAME_NOT_FOUND: usize = 69;
pub const ROLE_NET_UNAVAILABLE: usize = 70;
pub const ROLE_NET_PERMISSION_DENIED: usize = 71;
pub const ROLE_NET_ADDRESS_IN_USE: usize = 72;
pub const ROLE_NET_CONNECTION_REFUSED: usize = 73;
pub const ROLE_NET_CONNECTION_RESET: usize = 74;
pub const ROLE_NET_NOT_CONNECTED: usize = 75;
pub const ROLE_NET_TIMED_OUT: usize = 76;
pub const ROLE_NET_CLOSED: usize = 77;
pub const ROLE_NET_LIMIT_EXCEEDED: usize = 78;
pub const ROLE_NET_UNSUPPORTED: usize = 79;
pub const ROLE_NET_FAILED: usize = 80;
pub const ROLE_TCP_READ: usize = 81;
pub const ROLE_TCP_READ_DATA: usize = 82;
pub const ROLE_TCP_READ_END: usize = 83;
pub const ROLE_SHUTDOWN: usize = 84;
pub const ROLE_SHUTDOWN_READ: usize = 85;
pub const ROLE_SHUTDOWN_WRITE: usize = 86;
pub const ROLE_SHUTDOWN_BOTH: usize = 87;
pub const ROLE_TCP_RESOURCE: usize = 88;
pub const ROLE_TCP_STREAM: usize = 89;
pub const ROLE_TCP_LISTENER: usize = 90;
pub const ROLE_TLS_ERROR: usize = 91;
pub const ROLE_TLS_INVALID_CONFIG: usize = 92;
pub const ROLE_TLS_HANDSHAKE: usize = 93;
pub const ROLE_TLS_CERTIFICATE: usize = 94;
pub const ROLE_TLS_PROTOCOL: usize = 95;
pub const ROLE_TLS_NETWORK: usize = 96;
pub const ROLE_TLS_CLOSED: usize = 97;
pub const ROLE_TLS_LIMIT_EXCEEDED: usize = 98;
pub const ROLE_TLS_STREAM: usize = 99;
pub const ROLE_LIST: usize = 100;
pub const ROLE_MAP: usize = 101;
pub const ROLE_ARTIFACT: usize = 102;
pub const ROLE_VERIFIED_MODULE: usize = 103;
pub const ROLE_SLOT_SPEC: usize = 104;
pub const ROLE_INSTANCE: usize = 105;
pub const ROLE_SLOT: usize = 106;
pub const ROLE_FUNCTION_DEF: usize = 107;
pub const ROLE_CODE_ERROR: usize = 108;
pub const ROLE_LINK_ENV: usize = 109;
pub const ROLE_COMPILE_ENV: usize = 110;
pub const ROLE_COMPILE_OPTIONS: usize = 111;
pub const ROLE_COMPILE_ERRORS: usize = 112;
pub const ROLE_SYNTAX_TREE: usize = 113;
pub const ROLE_SYNTAX_ELEMENT: usize = 114;
pub const ROLE_SYNTAX_NODE: usize = 115;
pub const ROLE_SYNTAX_TOKEN: usize = 116;
pub const ROLE_SYNTAX_TRIVIA: usize = 117;
pub const ROLE_SYNTAX_BUILDER: usize = 118;
pub const ROLE_PARSE_STATUS: usize = 119;
pub const ROLE_PARSE_COMPLETE: usize = 120;
pub const ROLE_PARSE_INCOMPLETE: usize = 121;
pub const ROLE_PARSE_INVALID: usize = 122;
pub const ROLE_SYNTAX_DIAGNOSTIC: usize = 123;
pub const ROLE_SYNTAX_PARSE: usize = 124;
pub const ROLE_DYN_VALUE: usize = 125;
pub const ROLE_CLASS_DEF: usize = 126;
pub const ROLE_FUNCTION_CODE: usize = 127;
pub const ROLE_CLASS_CODE: usize = 128;
pub const ROLE_DEFINITION_SOURCE: usize = 129;
pub const ROLE_SOURCE_RANGE: usize = 130;
pub const ROLE_CODE_LOCATION: usize = 131;
pub const ROLE_FUNCTION_BINDING: usize = 132;
pub const ROLE_CLASS_BINDING: usize = 133;
pub const ROLE_DEFINITION_SPEC: usize = 134;
pub const ROLE_SLOT_CHANGE: usize = 135;
pub const ROLE_DEFINITION_IDENTITY: usize = 136;
pub const ROLE_IO_ERROR_BROKEN_PIPE: usize = 137;
pub const ROLE_IO_ERROR_INVALID_INPUT: usize = 138;
pub const ROLE_IO_ERROR_LIMIT_EXCEEDED: usize = 139;
pub const ROLE_ENV_ERROR: usize = 140;
pub const ROLE_ENV_ERROR_INVALID_NAME: usize = 141;
pub const ROLE_ENV_ERROR_INVALID_ENCODING: usize = 142;
pub const ROLE_ENV_ERROR_PERMISSION_DENIED: usize = 143;
pub const ROLE_ENV_ERROR_FAILED: usize = 144;
pub const ROLE_ENTROPY_ERROR: usize = 145;
pub const ROLE_ENTROPY_ERROR_INVALID_INPUT: usize = 146;
pub const ROLE_ENTROPY_ERROR_LIMIT_EXCEEDED: usize = 147;
pub const ROLE_ENTROPY_ERROR_UNAVAILABLE: usize = 148;
pub const ROLE_ENTROPY_ERROR_FAILED: usize = 149;
pub const ROLE_TUPLE3: usize = 150;
pub const ROLE_TUPLE4: usize = 151;
pub const ROLE_TUPLE5: usize = 152;
pub const ROLE_TUPLE6: usize = 153;
pub const ROLE_TUPLE7: usize = 154;
pub const ROLE_TUPLE8: usize = 155;
pub const ROLE_TUPLE9: usize = 156;
pub const ROLE_TUPLE10: usize = 157;
pub const ROLE_TUPLE11: usize = 158;
pub const ROLE_TUPLE12: usize = 159;
pub const ROLE_TUPLE13: usize = 160;
pub const ROLE_TUPLE14: usize = 161;
pub const ROLE_TUPLE15: usize = 162;
pub const ROLE_TUPLE16: usize = 163;
pub const ROLE_UNIT: usize = 164;

/// The core role of immediate floating-point values.
pub const ROLE_FLOAT: usize = 165;
pub const ROLE_STD_STREAM: usize = 166;
pub const ROLE_STD_STREAM_INPUT: usize = 167;
pub const ROLE_STD_STREAM_OUTPUT: usize = 168;
pub const ROLE_STD_STREAM_ERROR: usize = 169;
pub const ROLE_TTY_SIZE: usize = 170;
pub const ROLE_TTY_ERROR: usize = 171;
pub const ROLE_TTY_ERROR_CLOSED: usize = 172;
pub const ROLE_TTY_ERROR_NOT_TERMINAL: usize = 173;
pub const ROLE_TTY_ERROR_BUSY: usize = 174;
pub const ROLE_TTY_ERROR_PERMISSION_DENIED: usize = 175;
pub const ROLE_TTY_ERROR_UNSUPPORTED: usize = 176;
pub const ROLE_TTY_ERROR_FAILED: usize = 177;
pub const ROLE_RAW_MODE: usize = 178;
pub const ROLE_SIGNAL_KIND: usize = 179;
pub const ROLE_SIGNAL_INTERRUPT: usize = 180;
pub const ROLE_SIGNAL_TERMINATE: usize = 181;
pub const ROLE_SIGNAL_ERROR: usize = 182;
pub const ROLE_SIGNAL_ERROR_CLOSED: usize = 183;
pub const ROLE_SIGNAL_ERROR_INVALID_INPUT: usize = 184;
pub const ROLE_SIGNAL_ERROR_BUSY: usize = 185;
pub const ROLE_SIGNAL_ERROR_UNSUPPORTED: usize = 186;
pub const ROLE_SIGNAL_ERROR_LIMIT_EXCEEDED: usize = 187;
pub const ROLE_SIGNAL_ERROR_FAILED: usize = 188;
pub const ROLE_SIGNAL_STREAM: usize = 189;
pub const ROLE_IO_ERROR_UNSUPPORTED: usize = 190;
pub const ROLE_FS_ERROR_INVALID_INPUT: usize = 191;
pub const ROLE_FS_ERROR_INVALID_ENCODING: usize = 192;
pub const ROLE_FS_ERROR_LIMIT_EXCEEDED: usize = 193;
pub const ROLE_FS_ERROR_NOT_FOUND: usize = 194;
pub const ROLE_FS_ERROR_ALREADY_EXISTS: usize = 195;
pub const ROLE_FS_ERROR_PERMISSION_DENIED: usize = 196;
pub const ROLE_FS_ERROR_NOT_DIRECTORY: usize = 197;
pub const ROLE_FS_ERROR_IS_DIRECTORY: usize = 198;
pub const ROLE_FS_ERROR_DIRECTORY_NOT_EMPTY: usize = 199;
pub const ROLE_FS_ERROR_CROSS_DEVICE: usize = 200;
pub const ROLE_FS_ERROR_UNSUPPORTED: usize = 201;
pub const ROLE_OPEN_CREATE_NEW: usize = 202;
pub const ROLE_FILE_KIND: usize = 203;
pub const ROLE_FILE_KIND_FILE: usize = 204;
pub const ROLE_FILE_KIND_DIRECTORY: usize = 205;
pub const ROLE_FILE_KIND_SYMLINK: usize = 206;
pub const ROLE_FILE_KIND_OTHER: usize = 207;
pub const ROLE_FILE_INFO: usize = 208;
pub const ROLE_DIR_ENTRY: usize = 209;
pub const ROLE_RENAME_MODE: usize = 210;
pub const ROLE_RENAME_NO_REPLACE: usize = 211;
pub const ROLE_RENAME_REPLACE: usize = 212;
pub const ROLE_PIPE_ERROR: usize = 213;
pub const ROLE_PIPE_ERROR_CLOSED: usize = 214;
pub const ROLE_PIPE_ERROR_BROKEN_PIPE: usize = 215;
pub const ROLE_PIPE_ERROR_INVALID_INPUT: usize = 216;
pub const ROLE_PIPE_ERROR_LIMIT_EXCEEDED: usize = 217;
pub const ROLE_PIPE_ERROR_FAILED: usize = 218;
pub const ROLE_PIPE_END: usize = 219;
pub const ROLE_PIPE_READER: usize = 220;
pub const ROLE_PIPE_WRITER: usize = 221;
pub const ROLE_CHILD_INPUT: usize = 222;
pub const ROLE_CHILD_INPUT_INHERIT: usize = 223;
pub const ROLE_CHILD_INPUT_NULL: usize = 224;
pub const ROLE_CHILD_INPUT_PIPE: usize = 225;
pub const ROLE_CHILD_OUTPUT: usize = 226;
pub const ROLE_CHILD_OUTPUT_INHERIT: usize = 227;
pub const ROLE_CHILD_OUTPUT_NULL: usize = 228;
pub const ROLE_CHILD_OUTPUT_PIPE: usize = 229;
pub const ROLE_CHILD_ENV: usize = 230;
pub const ROLE_CHILD_ENV_INHERIT: usize = 231;
pub const ROLE_CHILD_ENV_EXACT: usize = 232;
pub const ROLE_EXEC_SPEC: usize = 233;
pub const ROLE_CHILD_STATUS: usize = 234;
pub const ROLE_CHILD_STATUS_EXITED: usize = 235;
pub const ROLE_CHILD_STATUS_TERMINATED: usize = 236;
pub const ROLE_EXEC_ERROR: usize = 237;
pub const ROLE_EXEC_ERROR_CLOSED: usize = 238;
pub const ROLE_EXEC_ERROR_INVALID_INPUT: usize = 239;
pub const ROLE_EXEC_ERROR_LIMIT_EXCEEDED: usize = 240;
pub const ROLE_EXEC_ERROR_NOT_FOUND: usize = 241;
pub const ROLE_EXEC_ERROR_PERMISSION_DENIED: usize = 242;
pub const ROLE_EXEC_ERROR_UNSUPPORTED: usize = 243;
pub const ROLE_EXEC_ERROR_FAILED: usize = 244;
pub const ROLE_CHILD: usize = 245;
pub const ROLE_PIPE_ERROR_UNSUPPORTED: usize = 246;
pub const ROLE_UDP_DATAGRAM: usize = 247;
pub const ROLE_UDP_SOCKET: usize = 248;
pub const ROLE_CHILD_ENV_OVERLAY: usize = 249;
pub const ROLE_FILE_HANDLE: usize = 250;
pub const ROLE_BRANCH_ERROR: usize = 251;
pub const ROLE_BRANCH_RESOURCE_ACTIVE: usize = 252;
pub const ROLE_BRANCH_LIMIT_EXCEEDED: usize = 253;

/// The tuple carrier role for one supported arity.
pub fn tuple_role(arity: usize) -> Option<usize> {
    match arity {
        2 => Some(ROLE_TUPLE2),
        3..=16 => Some(ROLE_TUPLE3 + arity - 3),
        _ => None,
    }
}

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
    if let Some(arity) = label
        .strip_prefix("Tuple")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|arity| (2..=16).contains(arity))
    {
        return &mut layout.tuples[arity];
    }
    match label {
        "Int" => &mut layout.int,
        "Float" => &mut layout.float,
        "Bool" => &mut layout.boolean,
        "String" => &mut layout.string,
        "Text" => &mut layout.text,
        "Substring" => &mut layout.substring,
        "Char" => &mut layout.char_value,
        "Bytes" => &mut layout.bytes,
        "FileHandle" => &mut layout.file_handle,
        "StringBuilder" => &mut layout.string_builder,
        "ByteBuffer" => &mut layout.byte_buffer,
        "Unit" => &mut layout.unit,
        "Option" => &mut layout.option,
        "Option.Some" => &mut layout.option_some,
        "Option.None" => &mut layout.option_none,
        "Result" => &mut layout.result,
        "Result.Ok" => &mut layout.result_ok,
        "Result.Err" => &mut layout.result_err,
        "IoError" => &mut layout.io_error,
        "IoError.BrokenPipe" => &mut layout.io_error_broken_pipe,
        "IoError.InvalidInput" => &mut layout.io_error_invalid_input,
        "IoError.LimitExceeded" => &mut layout.io_error_limit_exceeded,
        "IoError.Unsupported" => &mut layout.io_error_unsupported,
        "IoError.Failed" => &mut layout.io_error_failed,
        "EnvError" => &mut layout.env_error,
        "EnvError.InvalidName" => &mut layout.env_error_invalid_name,
        "EnvError.InvalidEncoding" => &mut layout.env_error_invalid_encoding,
        "EnvError.PermissionDenied" => &mut layout.env_error_permission_denied,
        "EnvError.Failed" => &mut layout.env_error_failed,
        "EntropyError" => &mut layout.entropy_error,
        "EntropyError.InvalidInput" => &mut layout.entropy_error_invalid_input,
        "EntropyError.LimitExceeded" => &mut layout.entropy_error_limit_exceeded,
        "EntropyError.Unavailable" => &mut layout.entropy_error_unavailable,
        "EntropyError.Failed" => &mut layout.entropy_error_failed,
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
        "BranchError" => &mut layout.branch_error,
        "BranchError.ResourceActive" => &mut layout.branch_resource_active,
        "BranchError.BranchLimitExceeded" => &mut layout.branch_limit_exceeded,
        "FsError" => &mut layout.fs_error,
        "FsError.Closed" => &mut layout.fs_error_closed,
        "FsError.InvalidInput" => &mut layout.fs_error_invalid_input,
        "FsError.InvalidEncoding" => &mut layout.fs_error_invalid_encoding,
        "FsError.LimitExceeded" => &mut layout.fs_error_limit_exceeded,
        "FsError.NotFound" => &mut layout.fs_error_not_found,
        "FsError.AlreadyExists" => &mut layout.fs_error_already_exists,
        "FsError.PermissionDenied" => &mut layout.fs_error_permission_denied,
        "FsError.NotDirectory" => &mut layout.fs_error_not_directory,
        "FsError.IsDirectory" => &mut layout.fs_error_is_directory,
        "FsError.DirectoryNotEmpty" => &mut layout.fs_error_directory_not_empty,
        "FsError.CrossDevice" => &mut layout.fs_error_cross_device,
        "FsError.Unsupported" => &mut layout.fs_error_unsupported,
        "FsError.Failed" => &mut layout.fs_error_failed,
        "OpenOptions" => &mut layout.open_options,
        "OpenOptions.ReadOnly" => &mut layout.open_read_only,
        "OpenOptions.WriteOnly" => &mut layout.open_write_only,
        "OpenOptions.ReadWrite" => &mut layout.open_read_write,
        "OpenOptions.Create" => &mut layout.open_create,
        "OpenOptions.CreateTruncate" => &mut layout.open_create_truncate,
        "OpenOptions.CreateNew" => &mut layout.open_create_new,
        "OpenOptions.Append" => &mut layout.open_append,
        "SeekFrom" => &mut layout.seek_from,
        "SeekFrom.Start" => &mut layout.seek_start,
        "SeekFrom.Current" => &mut layout.seek_current,
        "SeekFrom.End" => &mut layout.seek_end,
        "FileKind" => &mut layout.file_kind,
        "FileKind.File" => &mut layout.file_kind_file,
        "FileKind.Directory" => &mut layout.file_kind_directory,
        "FileKind.Symlink" => &mut layout.file_kind_symlink,
        "FileKind.Other" => &mut layout.file_kind_other,
        "FileInfo" => &mut layout.file_info,
        "DirEntry" => &mut layout.dir_entry,
        "RenameMode" => &mut layout.rename_mode,
        "RenameMode.NoReplace" => &mut layout.rename_no_replace,
        "RenameMode.Replace" => &mut layout.rename_replace,
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
        "StdStream" => &mut layout.std_stream,
        "StdStream.Input" => &mut layout.std_stream_input,
        "StdStream.Output" => &mut layout.std_stream_output,
        "StdStream.Error" => &mut layout.std_stream_error,
        "TtySize" => &mut layout.tty_size,
        "TtyError" => &mut layout.tty_error,
        "TtyError.Closed" => &mut layout.tty_error_closed,
        "TtyError.NotTerminal" => &mut layout.tty_error_not_terminal,
        "TtyError.Busy" => &mut layout.tty_error_busy,
        "TtyError.PermissionDenied" => &mut layout.tty_error_permission_denied,
        "TtyError.Unsupported" => &mut layout.tty_error_unsupported,
        "TtyError.Failed" => &mut layout.tty_error_failed,
        "RawMode" => &mut layout.raw_mode,
        "SignalKind" => &mut layout.signal_kind,
        "SignalKind.Interrupt" => &mut layout.signal_interrupt,
        "SignalKind.Terminate" => &mut layout.signal_terminate,
        "SignalError" => &mut layout.signal_error,
        "SignalError.Closed" => &mut layout.signal_error_closed,
        "SignalError.InvalidInput" => &mut layout.signal_error_invalid_input,
        "SignalError.Busy" => &mut layout.signal_error_busy,
        "SignalError.Unsupported" => &mut layout.signal_error_unsupported,
        "SignalError.LimitExceeded" => &mut layout.signal_error_limit_exceeded,
        "SignalError.Failed" => &mut layout.signal_error_failed,
        "SignalStream" => &mut layout.signal_stream,
        "PipeError" => &mut layout.pipe_error,
        "PipeError.Closed" => &mut layout.pipe_error_closed,
        "PipeError.BrokenPipe" => &mut layout.pipe_error_broken_pipe,
        "PipeError.InvalidInput" => &mut layout.pipe_error_invalid_input,
        "PipeError.LimitExceeded" => &mut layout.pipe_error_limit_exceeded,
        "PipeError.Unsupported" => &mut layout.pipe_error_unsupported,
        "PipeError.Failed" => &mut layout.pipe_error_failed,
        "PipeEnd" => &mut layout.pipe_end,
        "PipeReader" => &mut layout.pipe_reader,
        "PipeWriter" => &mut layout.pipe_writer,
        "ChildInput" => &mut layout.child_input,
        "ChildInput.Inherit" => &mut layout.child_input_inherit,
        "ChildInput.Null" => &mut layout.child_input_null,
        "ChildInput.Pipe" => &mut layout.child_input_pipe,
        "ChildOutput" => &mut layout.child_output,
        "ChildOutput.Inherit" => &mut layout.child_output_inherit,
        "ChildOutput.Null" => &mut layout.child_output_null,
        "ChildOutput.Pipe" => &mut layout.child_output_pipe,
        "ChildEnv" => &mut layout.child_env,
        "ChildEnv.Inherit" => &mut layout.child_env_inherit,
        "ChildEnv.Exact" => &mut layout.child_env_exact,
        "ChildEnv.Overlay" => &mut layout.child_env_overlay,
        "ExecSpec" => &mut layout.exec_spec,
        "ChildStatus" => &mut layout.child_status,
        "ChildStatus.Exited" => &mut layout.child_status_exited,
        "ChildStatus.Terminated" => &mut layout.child_status_terminated,
        "ExecError" => &mut layout.exec_error,
        "ExecError.Closed" => &mut layout.exec_error_closed,
        "ExecError.InvalidInput" => &mut layout.exec_error_invalid_input,
        "ExecError.LimitExceeded" => &mut layout.exec_error_limit_exceeded,
        "ExecError.NotFound" => &mut layout.exec_error_not_found,
        "ExecError.PermissionDenied" => &mut layout.exec_error_permission_denied,
        "ExecError.Unsupported" => &mut layout.exec_error_unsupported,
        "ExecError.Failed" => &mut layout.exec_error_failed,
        "Child" => &mut layout.child,
        "UdpDatagram" => &mut layout.udp_datagram,
        "UdpSocket" => &mut layout.udp_socket,
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
    layout_from_roles(&module.core_roles)
}

/// Read one core layout from relocated namespace roles.
pub fn layout_from_roles(roles: &[u32; crate::CORE_ROLE_COUNT]) -> CoreLayout {
    let mut layout = CoreLayout::default();
    for (role, slot) in roles.iter().enumerate() {
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
