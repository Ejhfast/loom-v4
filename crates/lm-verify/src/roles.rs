//! The pinned core roles and the tables that state them.
//!
//! One part of the bytecode verifier. `lib.rs` holds the shared
//! context, the error type, and the entry points.

use super::*;

/// The core role indices. The order is `corepin::PINNED_LABELS`, and
/// the test `the_role_indices_match_the_pinned_labels` proves it.
pub(crate) const ROLE_OPTION: usize = 0;
pub(crate) const ROLE_OPTION_SOME: usize = 1;
pub(crate) const ROLE_OPTION_NONE: usize = 2;
pub(crate) const ROLE_RESULT: usize = 3;
pub(crate) const ROLE_RESULT_OK: usize = 4;
pub(crate) const ROLE_RESULT_ERR: usize = 5;
pub(crate) const ROLE_IO_ERROR: usize = 6;
pub(crate) const ROLE_IO_ERROR_FAILED: usize = 7;
pub(crate) const ROLE_STEP_EVENT: usize = 8;
pub(crate) const ROLE_STEP_RAN: usize = 9;
pub(crate) const ROLE_STEP_WAITING: usize = 10;
pub(crate) const ROLE_STEP_DONE: usize = 11;
pub(crate) const ROLE_STEP_FAULT: usize = 12;
pub(crate) const ROLE_DRIVE_EVENT: usize = 13;
pub(crate) const ROLE_DRIVE_ASKED: usize = 14;
pub(crate) const ROLE_DRIVE_DONE: usize = 15;
pub(crate) const ROLE_DRIVE_FAULT: usize = 16;
pub(crate) const ROLE_RECV: usize = 17;
pub(crate) const ROLE_RECV_MSG: usize = 18;
pub(crate) const ROLE_RECV_CLOSED: usize = 19;
pub(crate) const ROLE_SEND_RESULT: usize = 20;
pub(crate) const ROLE_SEND_SENT: usize = 21;
pub(crate) const ROLE_SEND_CLOSED: usize = 22;
pub(crate) const ROLE_SEND_FAULT: usize = 23;
pub(crate) const ROLE_PROC_ERROR: usize = 24;
pub(crate) const ROLE_PROC_ERROR_DEAD: usize = 25;
pub(crate) const ROLE_PROC_ERROR_NOT_PAUSED: usize = 26;
pub(crate) const ROLE_PROC_ERROR_ALREADY_PAUSED: usize = 27;
pub(crate) const ROLE_PROC_ERROR_IN_USE: usize = 28;
pub(crate) const ROLE_PROC_CLASS: usize = 29;
pub(crate) const ROLE_SNAPSHOT_ERROR: usize = 30;
pub(crate) const ROLE_SNAPSHOT_RESOURCE_ACTIVE: usize = 31;
pub(crate) const ROLE_SNAPSHOT_LIMIT_EXCEEDED: usize = 32;
pub(crate) const ROLE_SNAPSHOT_BAD_IMAGE: usize = 33;
pub(crate) const ROLE_RESTORE_ERROR: usize = 34;
pub(crate) const ROLE_RESTORE_LIMIT_EXCEEDED: usize = 35;
pub(crate) const ROLE_FS_ERROR: usize = 36;
pub(crate) const ROLE_FS_ERROR_CLOSED: usize = 37;
pub(crate) const ROLE_FS_ERROR_FAILED: usize = 38;
pub(crate) const ROLE_OPEN_OPTIONS: usize = 39;
pub(crate) const ROLE_OPEN_READ_ONLY: usize = 40;
pub(crate) const ROLE_OPEN_WRITE_ONLY: usize = 41;
pub(crate) const ROLE_OPEN_READ_WRITE: usize = 42;
pub(crate) const ROLE_OPEN_CREATE: usize = 43;
pub(crate) const ROLE_OPEN_CREATE_TRUNCATE: usize = 44;
pub(crate) const ROLE_OPEN_APPEND: usize = 45;
pub(crate) const ROLE_SEEK_FROM: usize = 46;
pub(crate) const ROLE_SEEK_START: usize = 47;
pub(crate) const ROLE_SEEK_CURRENT: usize = 48;
pub(crate) const ROLE_SEEK_END: usize = 49;
pub(crate) const ROLE_IP_ADDRESS: usize = 63;
pub(crate) const ROLE_IP_V4: usize = 64;
pub(crate) const ROLE_IP_V6: usize = 65;
pub(crate) const ROLE_SOCKET_ADDRESS: usize = 66;
pub(crate) const ROLE_NET_ERROR: usize = 67;
pub(crate) const ROLE_NET_INVALID_INPUT: usize = 68;
pub(crate) const ROLE_NET_NAME_NOT_FOUND: usize = 69;
pub(crate) const ROLE_NET_UNAVAILABLE: usize = 70;
pub(crate) const ROLE_NET_PERMISSION_DENIED: usize = 71;
pub(crate) const ROLE_NET_ADDRESS_IN_USE: usize = 72;
pub(crate) const ROLE_NET_CONNECTION_REFUSED: usize = 73;
pub(crate) const ROLE_NET_CONNECTION_RESET: usize = 74;
pub(crate) const ROLE_NET_NOT_CONNECTED: usize = 75;
pub(crate) const ROLE_NET_TIMED_OUT: usize = 76;
pub(crate) const ROLE_NET_CLOSED: usize = 77;
pub(crate) const ROLE_NET_LIMIT_EXCEEDED: usize = 78;
pub(crate) const ROLE_NET_UNSUPPORTED: usize = 79;
pub(crate) const ROLE_NET_FAILED: usize = 80;
pub(crate) const ROLE_TCP_READ: usize = 81;
pub(crate) const ROLE_TCP_READ_DATA: usize = 82;
pub(crate) const ROLE_TCP_READ_END: usize = 83;
pub(crate) const ROLE_SHUTDOWN: usize = 84;
pub(crate) const ROLE_SHUTDOWN_READ: usize = 85;
pub(crate) const ROLE_SHUTDOWN_WRITE: usize = 86;
pub(crate) const ROLE_SHUTDOWN_BOTH: usize = 87;
pub(crate) const ROLE_TCP_RESOURCE: usize = 88;
pub(crate) const ROLE_TCP_STREAM: usize = 89;
pub(crate) const ROLE_TCP_LISTENER: usize = 90;
pub(crate) const ROLE_TLS_ERROR: usize = 91;
pub(crate) const ROLE_TLS_INVALID_CONFIG: usize = 92;
pub(crate) const ROLE_TLS_HANDSHAKE: usize = 93;
pub(crate) const ROLE_TLS_CERTIFICATE: usize = 94;
pub(crate) const ROLE_TLS_PROTOCOL: usize = 95;
pub(crate) const ROLE_TLS_NETWORK: usize = 96;
pub(crate) const ROLE_TLS_CLOSED: usize = 97;
pub(crate) const ROLE_TLS_LIMIT_EXCEEDED: usize = 98;
pub(crate) const ROLE_PARSE_STATUS: usize = 119;
pub(crate) const ROLE_PARSE_COMPLETE: usize = 120;
pub(crate) const ROLE_PARSE_INCOMPLETE: usize = 121;
pub(crate) const ROLE_PARSE_INVALID: usize = 122;
pub(crate) const ROLE_IO_ERROR_BROKEN_PIPE: usize = 137;
pub(crate) const ROLE_IO_ERROR_INVALID_INPUT: usize = 138;
pub(crate) const ROLE_IO_ERROR_LIMIT_EXCEEDED: usize = 139;
pub(crate) const ROLE_ENV_ERROR: usize = 140;
pub(crate) const ROLE_ENV_ERROR_INVALID_NAME: usize = 141;
pub(crate) const ROLE_ENV_ERROR_INVALID_ENCODING: usize = 142;
pub(crate) const ROLE_ENV_ERROR_PERMISSION_DENIED: usize = 143;
pub(crate) const ROLE_ENV_ERROR_FAILED: usize = 144;
pub(crate) const ROLE_ENTROPY_ERROR: usize = 145;
pub(crate) const ROLE_ENTROPY_ERROR_INVALID_INPUT: usize = 146;
pub(crate) const ROLE_ENTROPY_ERROR_LIMIT_EXCEEDED: usize = 147;
pub(crate) const ROLE_ENTROPY_ERROR_UNAVAILABLE: usize = 148;
pub(crate) const ROLE_ENTROPY_ERROR_FAILED: usize = 149;
pub(crate) const ROLE_STD_STREAM: usize = 166;
pub(crate) const ROLE_STD_STREAM_INPUT: usize = 167;
pub(crate) const ROLE_STD_STREAM_OUTPUT: usize = 168;
pub(crate) const ROLE_STD_STREAM_ERROR: usize = 169;
pub(crate) const ROLE_TTY_ERROR: usize = 171;
pub(crate) const ROLE_TTY_ERROR_CLOSED: usize = 172;
pub(crate) const ROLE_TTY_ERROR_NOT_TERMINAL: usize = 173;
pub(crate) const ROLE_TTY_ERROR_BUSY: usize = 174;
pub(crate) const ROLE_TTY_ERROR_PERMISSION_DENIED: usize = 175;
pub(crate) const ROLE_TTY_ERROR_UNSUPPORTED: usize = 176;
pub(crate) const ROLE_TTY_ERROR_FAILED: usize = 177;
pub(crate) const ROLE_SIGNAL_KIND: usize = 179;
pub(crate) const ROLE_SIGNAL_INTERRUPT: usize = 180;
pub(crate) const ROLE_SIGNAL_TERMINATE: usize = 181;
pub(crate) const ROLE_SIGNAL_ERROR: usize = 182;
pub(crate) const ROLE_SIGNAL_ERROR_CLOSED: usize = 183;
pub(crate) const ROLE_SIGNAL_ERROR_INVALID_INPUT: usize = 184;
pub(crate) const ROLE_SIGNAL_ERROR_BUSY: usize = 185;
pub(crate) const ROLE_SIGNAL_ERROR_UNSUPPORTED: usize = 186;
pub(crate) const ROLE_SIGNAL_ERROR_LIMIT_EXCEEDED: usize = 187;
pub(crate) const ROLE_SIGNAL_ERROR_FAILED: usize = 188;
pub(crate) const ROLE_IO_ERROR_UNSUPPORTED: usize = 190;
pub(crate) const ROLE_FS_ERROR_INVALID_INPUT: usize = 191;
pub(crate) const ROLE_FS_ERROR_INVALID_ENCODING: usize = 192;
pub(crate) const ROLE_FS_ERROR_LIMIT_EXCEEDED: usize = 193;
pub(crate) const ROLE_FS_ERROR_NOT_FOUND: usize = 194;
pub(crate) const ROLE_FS_ERROR_ALREADY_EXISTS: usize = 195;
pub(crate) const ROLE_FS_ERROR_PERMISSION_DENIED: usize = 196;
pub(crate) const ROLE_FS_ERROR_NOT_DIRECTORY: usize = 197;
pub(crate) const ROLE_FS_ERROR_IS_DIRECTORY: usize = 198;
pub(crate) const ROLE_FS_ERROR_DIRECTORY_NOT_EMPTY: usize = 199;
pub(crate) const ROLE_FS_ERROR_CROSS_DEVICE: usize = 200;
pub(crate) const ROLE_FS_ERROR_UNSUPPORTED: usize = 201;
pub(crate) const ROLE_OPEN_CREATE_NEW: usize = 202;
pub(crate) const ROLE_FILE_KIND: usize = 203;
pub(crate) const ROLE_FILE_KIND_FILE: usize = 204;
pub(crate) const ROLE_FILE_KIND_DIRECTORY: usize = 205;
pub(crate) const ROLE_FILE_KIND_SYMLINK: usize = 206;
pub(crate) const ROLE_FILE_KIND_OTHER: usize = 207;
pub(crate) const ROLE_FILE_INFO: usize = 208;
pub(crate) const ROLE_DIR_ENTRY: usize = 209;
pub(crate) const ROLE_RENAME_MODE: usize = 210;
pub(crate) const ROLE_RENAME_NO_REPLACE: usize = 211;
pub(crate) const ROLE_RENAME_REPLACE: usize = 212;
pub(crate) const ROLE_PIPE_ERROR: usize = 213;
pub(crate) const ROLE_PIPE_ERROR_CLOSED: usize = 214;
pub(crate) const ROLE_PIPE_ERROR_BROKEN_PIPE: usize = 215;
pub(crate) const ROLE_PIPE_ERROR_INVALID_INPUT: usize = 216;
pub(crate) const ROLE_PIPE_ERROR_LIMIT_EXCEEDED: usize = 217;
pub(crate) const ROLE_PIPE_ERROR_FAILED: usize = 218;
pub(crate) const ROLE_PIPE_END: usize = 219;
pub(crate) const ROLE_PIPE_READER: usize = 220;
pub(crate) const ROLE_PIPE_WRITER: usize = 221;
pub(crate) const ROLE_CHILD_INPUT: usize = 222;
pub(crate) const ROLE_CHILD_INPUT_INHERIT: usize = 223;
pub(crate) const ROLE_CHILD_INPUT_NULL: usize = 224;
pub(crate) const ROLE_CHILD_INPUT_PIPE: usize = 225;
pub(crate) const ROLE_CHILD_OUTPUT: usize = 226;
pub(crate) const ROLE_CHILD_OUTPUT_INHERIT: usize = 227;
pub(crate) const ROLE_CHILD_OUTPUT_NULL: usize = 228;
pub(crate) const ROLE_CHILD_OUTPUT_PIPE: usize = 229;
pub(crate) const ROLE_CHILD_ENV: usize = 230;
pub(crate) const ROLE_CHILD_ENV_INHERIT: usize = 231;
pub(crate) const ROLE_CHILD_ENV_EXACT: usize = 232;
pub(crate) const ROLE_EXEC_SPEC: usize = 233;
pub(crate) const ROLE_CHILD_STATUS: usize = 234;
pub(crate) const ROLE_CHILD_STATUS_EXITED: usize = 235;
pub(crate) const ROLE_CHILD_STATUS_TERMINATED: usize = 236;
pub(crate) const ROLE_EXEC_ERROR: usize = 237;
pub(crate) const ROLE_EXEC_ERROR_CLOSED: usize = 238;
pub(crate) const ROLE_EXEC_ERROR_INVALID_INPUT: usize = 239;
pub(crate) const ROLE_EXEC_ERROR_LIMIT_EXCEEDED: usize = 240;
pub(crate) const ROLE_EXEC_ERROR_NOT_FOUND: usize = 241;
pub(crate) const ROLE_EXEC_ERROR_PERMISSION_DENIED: usize = 242;
pub(crate) const ROLE_EXEC_ERROR_UNSUPPORTED: usize = 243;
pub(crate) const ROLE_EXEC_ERROR_FAILED: usize = 244;
pub(crate) const ROLE_PIPE_ERROR_UNSUPPORTED: usize = 246;
pub(crate) const ROLE_CHILD_ENV_OVERLAY: usize = 249;
pub(crate) const ROLE_FILE_HANDLE: usize = 250;
pub(crate) const ROLE_BRANCH_ERROR: usize = 251;
pub(crate) const ROLE_BRANCH_RESOURCE_ACTIVE: usize = 252;
pub(crate) const ROLE_BRANCH_LIMIT_EXCEEDED: usize = 253;

/// The field shape one core arm must carry.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldShape {
    /// The type variable at this position of the family arity.
    Var(u32),
    Str,
    Int,
    Bytes,
    Fault,
    Request,
    /// A list of integers, for example the bounded machine path of
    /// `SnapshotError.ResourceActive`.
    ListInt,
    NetError,
    PipeReader,
    PipeWriter,
    MapStrStr,
}

/// One core family: the parent role, the generic arity, and the arm
/// roles in declaration order.
const CORE_FAMILIES: [(usize, u32, &[usize], &str); 34] = [
    (
        ROLE_OPTION,
        1,
        &[ROLE_OPTION_SOME, ROLE_OPTION_NONE],
        "Option",
    ),
    (ROLE_RESULT, 2, &[ROLE_RESULT_OK, ROLE_RESULT_ERR], "Result"),
    (
        ROLE_IO_ERROR,
        0,
        &[
            ROLE_IO_ERROR_BROKEN_PIPE,
            ROLE_IO_ERROR_INVALID_INPUT,
            ROLE_IO_ERROR_LIMIT_EXCEEDED,
            ROLE_IO_ERROR_UNSUPPORTED,
            ROLE_IO_ERROR_FAILED,
        ],
        "IoError",
    ),
    (
        ROLE_ENV_ERROR,
        0,
        &[
            ROLE_ENV_ERROR_INVALID_NAME,
            ROLE_ENV_ERROR_INVALID_ENCODING,
            ROLE_ENV_ERROR_PERMISSION_DENIED,
            ROLE_ENV_ERROR_FAILED,
        ],
        "EnvError",
    ),
    (
        ROLE_ENTROPY_ERROR,
        0,
        &[
            ROLE_ENTROPY_ERROR_INVALID_INPUT,
            ROLE_ENTROPY_ERROR_LIMIT_EXCEEDED,
            ROLE_ENTROPY_ERROR_UNAVAILABLE,
            ROLE_ENTROPY_ERROR_FAILED,
        ],
        "EntropyError",
    ),
    (
        ROLE_STEP_EVENT,
        1,
        &[
            ROLE_STEP_RAN,
            ROLE_STEP_WAITING,
            ROLE_STEP_DONE,
            ROLE_STEP_FAULT,
        ],
        "StepEvent",
    ),
    (
        ROLE_DRIVE_EVENT,
        1,
        &[ROLE_DRIVE_ASKED, ROLE_DRIVE_DONE, ROLE_DRIVE_FAULT],
        "DriveEvent",
    ),
    (ROLE_RECV, 1, &[ROLE_RECV_MSG, ROLE_RECV_CLOSED], "Recv"),
    (
        ROLE_SEND_RESULT,
        0,
        &[ROLE_SEND_SENT, ROLE_SEND_CLOSED, ROLE_SEND_FAULT],
        "SendResult",
    ),
    (
        ROLE_PROC_ERROR,
        0,
        &[
            ROLE_PROC_ERROR_DEAD,
            ROLE_PROC_ERROR_NOT_PAUSED,
            ROLE_PROC_ERROR_ALREADY_PAUSED,
            ROLE_PROC_ERROR_IN_USE,
        ],
        "ProcError",
    ),
    (
        ROLE_SNAPSHOT_ERROR,
        0,
        &[
            ROLE_SNAPSHOT_RESOURCE_ACTIVE,
            ROLE_SNAPSHOT_LIMIT_EXCEEDED,
            ROLE_SNAPSHOT_BAD_IMAGE,
        ],
        "SnapshotError",
    ),
    (
        ROLE_RESTORE_ERROR,
        0,
        &[ROLE_RESTORE_LIMIT_EXCEEDED],
        "RestoreError",
    ),
    (
        ROLE_BRANCH_ERROR,
        0,
        &[ROLE_BRANCH_RESOURCE_ACTIVE, ROLE_BRANCH_LIMIT_EXCEEDED],
        "BranchError",
    ),
    (
        ROLE_FS_ERROR,
        0,
        &[
            ROLE_FS_ERROR_CLOSED,
            ROLE_FS_ERROR_INVALID_INPUT,
            ROLE_FS_ERROR_INVALID_ENCODING,
            ROLE_FS_ERROR_LIMIT_EXCEEDED,
            ROLE_FS_ERROR_NOT_FOUND,
            ROLE_FS_ERROR_ALREADY_EXISTS,
            ROLE_FS_ERROR_PERMISSION_DENIED,
            ROLE_FS_ERROR_NOT_DIRECTORY,
            ROLE_FS_ERROR_IS_DIRECTORY,
            ROLE_FS_ERROR_DIRECTORY_NOT_EMPTY,
            ROLE_FS_ERROR_CROSS_DEVICE,
            ROLE_FS_ERROR_UNSUPPORTED,
            ROLE_FS_ERROR_FAILED,
        ],
        "FsError",
    ),
    (
        ROLE_OPEN_OPTIONS,
        0,
        &[
            ROLE_OPEN_READ_ONLY,
            ROLE_OPEN_WRITE_ONLY,
            ROLE_OPEN_READ_WRITE,
            ROLE_OPEN_CREATE,
            ROLE_OPEN_CREATE_TRUNCATE,
            ROLE_OPEN_CREATE_NEW,
            ROLE_OPEN_APPEND,
        ],
        "OpenOptions",
    ),
    (
        ROLE_SEEK_FROM,
        0,
        &[ROLE_SEEK_START, ROLE_SEEK_CURRENT, ROLE_SEEK_END],
        "SeekFrom",
    ),
    (
        ROLE_FILE_KIND,
        0,
        &[
            ROLE_FILE_KIND_FILE,
            ROLE_FILE_KIND_DIRECTORY,
            ROLE_FILE_KIND_SYMLINK,
            ROLE_FILE_KIND_OTHER,
        ],
        "FileKind",
    ),
    (
        ROLE_RENAME_MODE,
        0,
        &[ROLE_RENAME_NO_REPLACE, ROLE_RENAME_REPLACE],
        "RenameMode",
    ),
    (ROLE_IP_ADDRESS, 0, &[ROLE_IP_V4, ROLE_IP_V6], "IpAddress"),
    (
        ROLE_NET_ERROR,
        0,
        &[
            ROLE_NET_INVALID_INPUT,
            ROLE_NET_NAME_NOT_FOUND,
            ROLE_NET_UNAVAILABLE,
            ROLE_NET_PERMISSION_DENIED,
            ROLE_NET_ADDRESS_IN_USE,
            ROLE_NET_CONNECTION_REFUSED,
            ROLE_NET_CONNECTION_RESET,
            ROLE_NET_NOT_CONNECTED,
            ROLE_NET_TIMED_OUT,
            ROLE_NET_CLOSED,
            ROLE_NET_LIMIT_EXCEEDED,
            ROLE_NET_UNSUPPORTED,
            ROLE_NET_FAILED,
        ],
        "NetError",
    ),
    (
        ROLE_TCP_READ,
        0,
        &[ROLE_TCP_READ_DATA, ROLE_TCP_READ_END],
        "TcpRead",
    ),
    (
        ROLE_SHUTDOWN,
        0,
        &[ROLE_SHUTDOWN_READ, ROLE_SHUTDOWN_WRITE, ROLE_SHUTDOWN_BOTH],
        "Shutdown",
    ),
    (
        ROLE_TLS_ERROR,
        0,
        &[
            ROLE_TLS_INVALID_CONFIG,
            ROLE_TLS_HANDSHAKE,
            ROLE_TLS_CERTIFICATE,
            ROLE_TLS_PROTOCOL,
            ROLE_TLS_NETWORK,
            ROLE_TLS_CLOSED,
            ROLE_TLS_LIMIT_EXCEEDED,
        ],
        "TlsError",
    ),
    (
        ROLE_PARSE_STATUS,
        0,
        &[
            ROLE_PARSE_COMPLETE,
            ROLE_PARSE_INCOMPLETE,
            ROLE_PARSE_INVALID,
        ],
        "ParseStatus",
    ),
    (
        ROLE_STD_STREAM,
        0,
        &[
            ROLE_STD_STREAM_INPUT,
            ROLE_STD_STREAM_OUTPUT,
            ROLE_STD_STREAM_ERROR,
        ],
        "StdStream",
    ),
    (
        ROLE_TTY_ERROR,
        0,
        &[
            ROLE_TTY_ERROR_CLOSED,
            ROLE_TTY_ERROR_NOT_TERMINAL,
            ROLE_TTY_ERROR_BUSY,
            ROLE_TTY_ERROR_PERMISSION_DENIED,
            ROLE_TTY_ERROR_UNSUPPORTED,
            ROLE_TTY_ERROR_FAILED,
        ],
        "TtyError",
    ),
    (
        ROLE_SIGNAL_KIND,
        0,
        &[ROLE_SIGNAL_INTERRUPT, ROLE_SIGNAL_TERMINATE],
        "SignalKind",
    ),
    (
        ROLE_SIGNAL_ERROR,
        0,
        &[
            ROLE_SIGNAL_ERROR_CLOSED,
            ROLE_SIGNAL_ERROR_INVALID_INPUT,
            ROLE_SIGNAL_ERROR_BUSY,
            ROLE_SIGNAL_ERROR_UNSUPPORTED,
            ROLE_SIGNAL_ERROR_LIMIT_EXCEEDED,
            ROLE_SIGNAL_ERROR_FAILED,
        ],
        "SignalError",
    ),
    (
        ROLE_PIPE_ERROR,
        0,
        &[
            ROLE_PIPE_ERROR_CLOSED,
            ROLE_PIPE_ERROR_BROKEN_PIPE,
            ROLE_PIPE_ERROR_INVALID_INPUT,
            ROLE_PIPE_ERROR_LIMIT_EXCEEDED,
            ROLE_PIPE_ERROR_UNSUPPORTED,
            ROLE_PIPE_ERROR_FAILED,
        ],
        "PipeError",
    ),
    (
        ROLE_CHILD_INPUT,
        0,
        &[
            ROLE_CHILD_INPUT_INHERIT,
            ROLE_CHILD_INPUT_NULL,
            ROLE_CHILD_INPUT_PIPE,
        ],
        "ChildInput",
    ),
    (
        ROLE_CHILD_OUTPUT,
        0,
        &[
            ROLE_CHILD_OUTPUT_INHERIT,
            ROLE_CHILD_OUTPUT_NULL,
            ROLE_CHILD_OUTPUT_PIPE,
        ],
        "ChildOutput",
    ),
    (
        ROLE_CHILD_ENV,
        0,
        &[
            ROLE_CHILD_ENV_INHERIT,
            ROLE_CHILD_ENV_EXACT,
            ROLE_CHILD_ENV_OVERLAY,
        ],
        "ChildEnv",
    ),
    (
        ROLE_CHILD_STATUS,
        0,
        &[ROLE_CHILD_STATUS_EXITED, ROLE_CHILD_STATUS_TERMINATED],
        "ChildStatus",
    ),
    (
        ROLE_EXEC_ERROR,
        0,
        &[
            ROLE_EXEC_ERROR_CLOSED,
            ROLE_EXEC_ERROR_INVALID_INPUT,
            ROLE_EXEC_ERROR_LIMIT_EXCEEDED,
            ROLE_EXEC_ERROR_NOT_FOUND,
            ROLE_EXEC_ERROR_PERMISSION_DENIED,
            ROLE_EXEC_ERROR_UNSUPPORTED,
            ROLE_EXEC_ERROR_FAILED,
        ],
        "ExecError",
    ),
];

/// The field layout every core arm must carry, by role.
const CORE_ARM_FIELDS: [(usize, &[FieldShape]); 139] = [
    (ROLE_OPTION_SOME, &[FieldShape::Var(0)]),
    (ROLE_OPTION_NONE, &[]),
    (ROLE_RESULT_OK, &[FieldShape::Var(0)]),
    (ROLE_RESULT_ERR, &[FieldShape::Var(1)]),
    (ROLE_IO_ERROR_BROKEN_PIPE, &[]),
    (ROLE_IO_ERROR_INVALID_INPUT, &[FieldShape::Str]),
    (ROLE_IO_ERROR_LIMIT_EXCEEDED, &[FieldShape::Str]),
    (ROLE_IO_ERROR_UNSUPPORTED, &[FieldShape::Str]),
    (ROLE_IO_ERROR_FAILED, &[FieldShape::Str]),
    (ROLE_ENV_ERROR_INVALID_NAME, &[FieldShape::Str]),
    (ROLE_ENV_ERROR_INVALID_ENCODING, &[FieldShape::Str]),
    (ROLE_ENV_ERROR_PERMISSION_DENIED, &[FieldShape::Str]),
    (ROLE_ENV_ERROR_FAILED, &[FieldShape::Str]),
    (ROLE_ENTROPY_ERROR_INVALID_INPUT, &[FieldShape::Str]),
    (ROLE_ENTROPY_ERROR_LIMIT_EXCEEDED, &[FieldShape::Str]),
    (ROLE_ENTROPY_ERROR_UNAVAILABLE, &[FieldShape::Str]),
    (ROLE_ENTROPY_ERROR_FAILED, &[FieldShape::Str]),
    (ROLE_STEP_RAN, &[]),
    (ROLE_STEP_WAITING, &[]),
    (ROLE_STEP_DONE, &[FieldShape::Var(0)]),
    (ROLE_STEP_FAULT, &[FieldShape::Fault]),
    (ROLE_DRIVE_ASKED, &[FieldShape::Request]),
    (ROLE_DRIVE_DONE, &[FieldShape::Var(0)]),
    (ROLE_DRIVE_FAULT, &[FieldShape::Fault]),
    (ROLE_RECV_MSG, &[FieldShape::Var(0)]),
    (ROLE_RECV_CLOSED, &[]),
    (ROLE_SEND_SENT, &[]),
    (ROLE_SEND_CLOSED, &[]),
    (ROLE_SEND_FAULT, &[FieldShape::Fault]),
    (ROLE_PROC_ERROR_DEAD, &[]),
    (ROLE_PROC_ERROR_NOT_PAUSED, &[]),
    (ROLE_PROC_ERROR_ALREADY_PAUSED, &[]),
    (ROLE_PROC_ERROR_IN_USE, &[]),
    (
        ROLE_SNAPSHOT_RESOURCE_ACTIVE,
        &[FieldShape::ListInt, FieldShape::Str],
    ),
    (ROLE_SNAPSHOT_LIMIT_EXCEEDED, &[]),
    (ROLE_SNAPSHOT_BAD_IMAGE, &[FieldShape::Str]),
    (ROLE_RESTORE_LIMIT_EXCEEDED, &[]),
    (
        ROLE_BRANCH_RESOURCE_ACTIVE,
        &[FieldShape::ListInt, FieldShape::Str],
    ),
    (ROLE_BRANCH_LIMIT_EXCEEDED, &[]),
    (ROLE_FS_ERROR_CLOSED, &[]),
    (ROLE_FS_ERROR_INVALID_INPUT, &[FieldShape::Str]),
    (ROLE_FS_ERROR_INVALID_ENCODING, &[FieldShape::Str]),
    (ROLE_FS_ERROR_LIMIT_EXCEEDED, &[FieldShape::Str]),
    (ROLE_FS_ERROR_NOT_FOUND, &[FieldShape::Str]),
    (ROLE_FS_ERROR_ALREADY_EXISTS, &[FieldShape::Str]),
    (ROLE_FS_ERROR_PERMISSION_DENIED, &[FieldShape::Str]),
    (ROLE_FS_ERROR_NOT_DIRECTORY, &[FieldShape::Str]),
    (ROLE_FS_ERROR_IS_DIRECTORY, &[FieldShape::Str]),
    (ROLE_FS_ERROR_DIRECTORY_NOT_EMPTY, &[FieldShape::Str]),
    (ROLE_FS_ERROR_CROSS_DEVICE, &[FieldShape::Str]),
    (ROLE_FS_ERROR_UNSUPPORTED, &[FieldShape::Str]),
    (ROLE_FS_ERROR_FAILED, &[FieldShape::Str]),
    (ROLE_OPEN_READ_ONLY, &[]),
    (ROLE_OPEN_WRITE_ONLY, &[]),
    (ROLE_OPEN_READ_WRITE, &[]),
    (ROLE_OPEN_CREATE, &[]),
    (ROLE_OPEN_CREATE_TRUNCATE, &[]),
    (ROLE_OPEN_CREATE_NEW, &[]),
    (ROLE_OPEN_APPEND, &[]),
    (ROLE_SEEK_START, &[FieldShape::Int]),
    (ROLE_SEEK_CURRENT, &[FieldShape::Int]),
    (ROLE_SEEK_END, &[FieldShape::Int]),
    (ROLE_FILE_KIND_FILE, &[]),
    (ROLE_FILE_KIND_DIRECTORY, &[]),
    (ROLE_FILE_KIND_SYMLINK, &[]),
    (ROLE_FILE_KIND_OTHER, &[]),
    (ROLE_RENAME_NO_REPLACE, &[]),
    (ROLE_RENAME_REPLACE, &[]),
    (ROLE_IP_V4, &[FieldShape::Bytes]),
    (ROLE_IP_V6, &[FieldShape::Bytes]),
    (ROLE_NET_INVALID_INPUT, &[FieldShape::Str]),
    (ROLE_NET_NAME_NOT_FOUND, &[FieldShape::Str]),
    (ROLE_NET_UNAVAILABLE, &[FieldShape::Str]),
    (ROLE_NET_PERMISSION_DENIED, &[FieldShape::Str]),
    (ROLE_NET_ADDRESS_IN_USE, &[FieldShape::Str]),
    (ROLE_NET_CONNECTION_REFUSED, &[FieldShape::Str]),
    (ROLE_NET_CONNECTION_RESET, &[FieldShape::Str]),
    (ROLE_NET_NOT_CONNECTED, &[FieldShape::Str]),
    (ROLE_NET_TIMED_OUT, &[FieldShape::Str]),
    (ROLE_NET_CLOSED, &[]),
    (ROLE_NET_LIMIT_EXCEEDED, &[FieldShape::Str]),
    (ROLE_NET_UNSUPPORTED, &[FieldShape::Str]),
    (ROLE_NET_FAILED, &[FieldShape::Str]),
    (ROLE_TCP_READ_DATA, &[FieldShape::Bytes]),
    (ROLE_TCP_READ_END, &[]),
    (ROLE_SHUTDOWN_READ, &[]),
    (ROLE_SHUTDOWN_WRITE, &[]),
    (ROLE_SHUTDOWN_BOTH, &[]),
    (ROLE_TLS_INVALID_CONFIG, &[FieldShape::Str]),
    (ROLE_TLS_HANDSHAKE, &[FieldShape::Str]),
    (ROLE_TLS_CERTIFICATE, &[FieldShape::Str]),
    (ROLE_TLS_PROTOCOL, &[FieldShape::Str]),
    (ROLE_TLS_NETWORK, &[FieldShape::NetError]),
    (ROLE_TLS_CLOSED, &[]),
    (ROLE_TLS_LIMIT_EXCEEDED, &[FieldShape::Str]),
    (ROLE_PARSE_COMPLETE, &[]),
    (ROLE_PARSE_INCOMPLETE, &[]),
    (ROLE_PARSE_INVALID, &[]),
    (ROLE_STD_STREAM_INPUT, &[]),
    (ROLE_STD_STREAM_OUTPUT, &[]),
    (ROLE_STD_STREAM_ERROR, &[]),
    (ROLE_TTY_ERROR_CLOSED, &[]),
    (ROLE_TTY_ERROR_NOT_TERMINAL, &[]),
    (ROLE_TTY_ERROR_BUSY, &[]),
    (ROLE_TTY_ERROR_PERMISSION_DENIED, &[FieldShape::Str]),
    (ROLE_TTY_ERROR_UNSUPPORTED, &[FieldShape::Str]),
    (ROLE_TTY_ERROR_FAILED, &[FieldShape::Str]),
    (ROLE_SIGNAL_INTERRUPT, &[]),
    (ROLE_SIGNAL_TERMINATE, &[]),
    (ROLE_SIGNAL_ERROR_CLOSED, &[]),
    (ROLE_SIGNAL_ERROR_INVALID_INPUT, &[FieldShape::Str]),
    (ROLE_SIGNAL_ERROR_BUSY, &[]),
    (ROLE_SIGNAL_ERROR_UNSUPPORTED, &[FieldShape::Str]),
    (ROLE_SIGNAL_ERROR_LIMIT_EXCEEDED, &[FieldShape::Str]),
    (ROLE_SIGNAL_ERROR_FAILED, &[FieldShape::Str]),
    (ROLE_PIPE_ERROR_CLOSED, &[]),
    (ROLE_PIPE_ERROR_BROKEN_PIPE, &[]),
    (ROLE_PIPE_ERROR_INVALID_INPUT, &[FieldShape::Str]),
    (ROLE_PIPE_ERROR_LIMIT_EXCEEDED, &[FieldShape::Str]),
    (ROLE_PIPE_ERROR_UNSUPPORTED, &[FieldShape::Str]),
    (ROLE_PIPE_ERROR_FAILED, &[FieldShape::Str]),
    (ROLE_CHILD_INPUT_INHERIT, &[]),
    (ROLE_CHILD_INPUT_NULL, &[]),
    (ROLE_CHILD_INPUT_PIPE, &[FieldShape::PipeReader]),
    (ROLE_CHILD_OUTPUT_INHERIT, &[]),
    (ROLE_CHILD_OUTPUT_NULL, &[]),
    (ROLE_CHILD_OUTPUT_PIPE, &[FieldShape::PipeWriter]),
    (ROLE_CHILD_ENV_INHERIT, &[]),
    (ROLE_CHILD_ENV_EXACT, &[FieldShape::MapStrStr]),
    (ROLE_CHILD_ENV_OVERLAY, &[FieldShape::MapStrStr]),
    (ROLE_CHILD_STATUS_EXITED, &[FieldShape::Int]),
    (ROLE_CHILD_STATUS_TERMINATED, &[]),
    (ROLE_EXEC_ERROR_CLOSED, &[]),
    (ROLE_EXEC_ERROR_INVALID_INPUT, &[FieldShape::Str]),
    (ROLE_EXEC_ERROR_LIMIT_EXCEEDED, &[FieldShape::Str]),
    (ROLE_EXEC_ERROR_NOT_FOUND, &[FieldShape::Str]),
    (ROLE_EXEC_ERROR_PERMISSION_DENIED, &[FieldShape::Str]),
    (ROLE_EXEC_ERROR_UNSUPPORTED, &[FieldShape::Str]),
    (ROLE_EXEC_ERROR_FAILED, &[FieldShape::Str]),
];

/// Prove the shape of every declared core role slot.
///
/// The artifact declares which class fills each role. The verifier
/// never trusts that claim: it proves the kind, the generic arity, the
/// parent slot, and the exact field layout of every filled slot. A
/// crafted table therefore rejects instead of handing the runtime a
/// class it cannot allocate through.
///
/// The rules read structure only. No name and no definition hash takes
/// part, so a rename changes nothing the verifier reads.
pub(crate) fn verify_core_roles(module: &Module) -> Result<(), VerifyError> {
    let terr = |message: String| VerifyError {
        func: None,
        message,
    };
    let slot = |role: usize| -> Option<u32> {
        let idx = module.core_roles[role];
        if idx == lm_bytecode::NO_ROLE {
            None
        } else {
            Some(idx)
        }
    };
    // A role slot names a class of this module, and no two roles name
    // one class. The decoder proves the same rule; a hand-built module
    // reaches the verifier without a decoder.
    let mut taken: Vec<(u32, usize)> = Vec::new();
    for role in 0..lm_bytecode::CORE_ROLE_COUNT {
        let Some(idx) = slot(role) else { continue };
        if idx as usize >= module.classes.len() {
            let label = lm_bytecode::corepin::PINNED_LABELS[role];
            return Err(terr(format!(
                "the core role `{label}` (table {role}) names a class outside the class table"
            )));
        }
        if let Some((_, first_role)) = taken.iter().find(|(class, _)| *class == idx) {
            let first_label = lm_bytecode::corepin::PINNED_LABELS[*first_role];
            let label = lm_bytecode::corepin::PINNED_LABELS[role];
            return Err(terr(format!(
                "the core roles `{first_label}` (table {first_role}) and `{label}` \
                 (table {role}) name the same class"
            )));
        }
        taken.push((idx, role));
    }
    for (family_role, arity, arm_roles, family) in CORE_FAMILIES {
        let Some(parent) = slot(family_role) else {
            // A family the artifact does not declare must declare no
            // arm either. The runtime allocates through the arms.
            for arm in arm_roles {
                if slot(*arm).is_some() {
                    return Err(terr(format!(
                        "the core family `{family}` declares an arm without its parent"
                    )));
                }
            }
            continue;
        };
        let class = &module.classes[parent as usize];
        if class.kind != BcClassKind::Abstract
            || class.type_params != arity
            || class.parent().is_some()
            || !class.fields.is_empty()
        {
            return Err(terr(format!(
                "the core family `{family}` names a class that is not its enum parent"
            )));
        }
        for arm_role in arm_roles {
            let Some(arm) = slot(*arm_role) else {
                return Err(terr(format!(
                    "the core family `{family}` resolves without every arm"
                )));
            };
            let arm_class = &module.classes[arm as usize];
            if arm_class.kind != BcClassKind::Case
                || arm_class.type_params != arity
                || arm_class.parent() != Some(parent)
            {
                return Err(terr(format!(
                    "the core family `{family}` names an arm that is not its case class"
                )));
            }
            let fields = CORE_ARM_FIELDS
                .iter()
                .find(|(role, _)| role == arm_role)
                .map(|(_, fields)| *fields)
                .expect("every arm role states its field layout");
            if arm_class.fields.len() != fields.len() {
                return Err(terr(format!(
                    "the core family `{family}` names an arm with the wrong field count"
                )));
            }
            for (position, want) in fields.iter().enumerate() {
                let found = &module.types[arm_class.fields[position].1 as usize];
                let ok =
                    match want {
                        FieldShape::Var(i) => found == &BcType::Var(*i),
                        FieldShape::Str => found == &BcType::Str,
                        FieldShape::Int => found == &BcType::Int,
                        FieldShape::Bytes => found == &BcType::Bytes,
                        FieldShape::Fault => found == &BcType::Fault,
                        FieldShape::Request => found == &BcType::Request,
                        // The element index is read through `get`, because
                        // this pass must reject a crafted table instead of
                        // reaching outside the type table.
                        FieldShape::ListInt => match found {
                            BcType::List(elem) => {
                                module.types.get(*elem as usize) == Some(&BcType::Int)
                            }
                            _ => false,
                        },
                        FieldShape::NetError => {
                            slot(ROLE_NET_ERROR).is_some_and(|class| found == &BcType::Class(class))
                        }
                        FieldShape::PipeReader => slot(ROLE_PIPE_READER)
                            .is_some_and(|class| found == &BcType::Class(class)),
                        FieldShape::PipeWriter => slot(ROLE_PIPE_WRITER)
                            .is_some_and(|class| found == &BcType::Class(class)),
                        FieldShape::MapStrStr => match found {
                            BcType::Map(key, value) => {
                                module.types.get(*key as usize) == Some(&BcType::Str)
                                    && module.types.get(*value as usize) == Some(&BcType::Str)
                            }
                            _ => false,
                        },
                    };
                if !ok {
                    return Err(terr(format!(
                        "the core family `{family}` names an arm whose field {position} \
                         has the wrong type"
                    )));
                }
            }
        }
    }
    // The proc class is not an enum family. It is one ordinary generic
    // class with one type parameter, no parent, and no field. The
    // mailbox rules of `Proc.Spawn` and `Proc.Recv` read the class
    // table through it, so its shape is proved here.
    if let Some(idx) = slot(ROLE_PROC_CLASS) {
        let class = &module.classes[idx as usize];
        if class.kind != BcClassKind::Normal
            || class.type_params != 1
            || class.parent().is_some()
            || !class.fields.is_empty()
        {
            return Err(terr(
                "the core class `Proc` names a class that is not the proc parent".to_string(),
            ));
        }
    }
    for arity in 2..=16 {
        let role = lm_bytecode::corepin::tuple_role(arity)
            .expect("every supported tuple arity has one role");
        let Some(idx) = slot(role) else { continue };
        let class = &module.classes[idx as usize];
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != arity as u32
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !class.fields.is_empty()
        {
            return Err(terr(format!(
                "the core role `Tuple{arity}` does not name its native tuple carrier"
            )));
        }
    }
    if let Some(idx) = slot(lm_bytecode::corepin::ROLE_UNIT) {
        let class = &module.classes[idx as usize];
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !class.fields.is_empty()
        {
            return Err(terr(
                "the core role `Unit` does not name its native unit carrier".to_string(),
            ));
        }
    }
    if let Some(idx) = slot(ROLE_SOCKET_ADDRESS) {
        let Some(ip) = slot(ROLE_IP_ADDRESS) else {
            return Err(terr(
                "the SocketAddress role requires the IpAddress role".to_string(),
            ));
        };
        let class = &module.classes[idx as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || fields.len() != 4
            || fields[0] != &BcType::Class(ip)
            || fields[1] != &BcType::Int
            || fields[2] != &BcType::Int
            || fields[3] != &BcType::Int
        {
            return Err(terr(
                "the SocketAddress role does not name its final value class".to_string(),
            ));
        }
    }
    if let Some(idx) = slot(lm_bytecode::corepin::ROLE_UDP_DATAGRAM) {
        let Some(address) = slot(ROLE_SOCKET_ADDRESS) else {
            return Err(terr(
                "the UdpDatagram role requires the SocketAddress role".to_string(),
            ));
        };
        let class = &module.classes[idx as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || fields != [&BcType::Bytes, &BcType::Class(address)]
        {
            return Err(terr(
                "the UdpDatagram role does not name its final value class".to_string(),
            ));
        }
    }
    let tcp_roles = [
        slot(ROLE_TCP_RESOURCE),
        slot(ROLE_TCP_STREAM),
        slot(ROLE_TCP_LISTENER),
    ];
    if tcp_roles.iter().any(Option::is_some) && tcp_roles.iter().any(Option::is_none) {
        return Err(terr(
            "the TCP resource family resolves without every class".to_string(),
        ));
    }
    if let [Some(resource), Some(stream), Some(listener)] = tcp_roles {
        let base = &module.classes[resource as usize];
        if base.kind != BcClassKind::Normal
            || base.is_final
            || base.type_params != 0
            || base.parent().is_some()
            || !base.parent_args.is_empty()
            || !base.fields.is_empty()
        {
            return Err(terr(
                "the TcpResource role does not name its stateless base class".to_string(),
            ));
        }
        for (idx, name) in [(stream, "TcpStream"), (listener, "TcpListener")] {
            let class = &module.classes[idx as usize];
            if class.kind != BcClassKind::Normal
                || !class.is_final
                || class.type_params != 0
                || class.parent() != Some(resource)
                || !class.parent_args.is_empty()
                || !class.fields.is_empty()
            {
                return Err(terr(format!(
                    "the {name} role does not name its final resource class"
                )));
            }
        }
        for (idx, class) in module.classes.iter().enumerate() {
            if class.parent() == Some(resource) && idx as u32 != stream && idx as u32 != listener {
                return Err(terr(
                    "a class other than TcpStream or TcpListener extends TcpResource".to_string(),
                ));
            }
        }
    }
    let pipe_roles = [
        slot(ROLE_PIPE_END),
        slot(ROLE_PIPE_READER),
        slot(ROLE_PIPE_WRITER),
    ];
    if pipe_roles.iter().any(Option::is_some) && pipe_roles.iter().any(Option::is_none) {
        return Err(terr(
            "the pipe resource family resolves without every class".to_string(),
        ));
    }
    if let [Some(resource), Some(reader), Some(writer)] = pipe_roles {
        let base = &module.classes[resource as usize];
        if base.kind != BcClassKind::Normal
            || base.is_final
            || base.type_params != 0
            || base.parent().is_some()
            || !base.parent_args.is_empty()
            || !base.fields.is_empty()
        {
            return Err(terr(
                "the PipeEnd role does not name its stateless base class".to_string(),
            ));
        }
        for (idx, name) in [(reader, "PipeReader"), (writer, "PipeWriter")] {
            let class = &module.classes[idx as usize];
            if class.kind != BcClassKind::Normal
                || !class.is_final
                || class.type_params != 0
                || class.parent() != Some(resource)
                || !class.parent_args.is_empty()
                || !class.fields.is_empty()
            {
                return Err(terr(format!(
                    "the {name} role does not name its final resource class"
                )));
            }
        }
        for (idx, class) in module.classes.iter().enumerate() {
            if class.parent() == Some(resource) && idx as u32 != reader && idx as u32 != writer {
                return Err(terr(
                    "a class other than PipeReader or PipeWriter extends PipeEnd".to_string(),
                ));
            }
        }
    }
    let native_roles = [
        (lm_bytecode::corepin::ROLE_INT, "Int"),
        (lm_bytecode::corepin::ROLE_FLOAT, "Float"),
        (lm_bytecode::corepin::ROLE_BOOL, "Bool"),
        (lm_bytecode::corepin::ROLE_BYTES, "Bytes"),
        (ROLE_FILE_HANDLE, "FileHandle"),
        (lm_bytecode::corepin::ROLE_STRING_BUILDER, "StringBuilder"),
        (lm_bytecode::corepin::ROLE_BYTE_BUFFER, "ByteBuffer"),
        (lm_bytecode::corepin::ROLE_CHAR, "Char"),
        (lm_bytecode::corepin::ROLE_TLS_STREAM, "TlsStream"),
        (lm_bytecode::corepin::ROLE_RAW_MODE, "RawMode"),
        (lm_bytecode::corepin::ROLE_SIGNAL_STREAM, "SignalStream"),
        (lm_bytecode::corepin::ROLE_CHILD, "Child"),
        (lm_bytecode::corepin::ROLE_UDP_SOCKET, "UdpSocket"),
        (lm_bytecode::corepin::ROLE_REGEX, "Regex"),
        (lm_bytecode::corepin::ROLE_REGEX_MATCH, "RegexMatch"),
        (lm_bytecode::corepin::ROLE_ARTIFACT, "Artifact"),
        (lm_bytecode::corepin::ROLE_VERIFIED_MODULE, "VerifiedModule"),
        (lm_bytecode::corepin::ROLE_CLASS_CODE, "ClassCode"),
        (lm_bytecode::corepin::ROLE_SLOT_SPEC, "SlotSpec"),
        (lm_bytecode::corepin::ROLE_INSTANCE, "Instance"),
        (lm_bytecode::corepin::ROLE_SLOT, "Slot"),
        (lm_bytecode::corepin::ROLE_SLOT_CHANGE, "SlotChange"),
        (lm_bytecode::corepin::ROLE_CLASS_DEF, "ClassDef"),
    ];
    for (role, name) in native_roles {
        let Some(idx) = slot(role) else { continue };
        let class = &module.classes[idx as usize];
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !class.fields.is_empty()
        {
            return Err(terr(format!(
                "the core role `{name}` does not name a final stateless class"
            )));
        }
    }
    if let Some(spec) = slot(ROLE_EXEC_SPEC) {
        let Some(option) = slot(ROLE_OPTION) else {
            return Err(terr(
                "the ExecSpec role requires the Option role".to_string(),
            ));
        };
        let Some(environment) = slot(ROLE_CHILD_ENV) else {
            return Err(terr(
                "the ExecSpec role requires the ChildEnv role".to_string(),
            ));
        };
        let Some(input) = slot(ROLE_CHILD_INPUT) else {
            return Err(terr(
                "the ExecSpec role requires the ChildInput role".to_string(),
            ));
        };
        let Some(output) = slot(ROLE_CHILD_OUTPUT) else {
            return Err(terr(
                "the ExecSpec role requires the ChildOutput role".to_string(),
            ));
        };
        let class = &module.classes[spec as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        let valid = matches!(fields.as_slice(), [BcType::Str, BcType::List(arguments), BcType::Inst(found_option, option_args), BcType::Class(found_env), BcType::Class(found_input), BcType::Class(found_output), BcType::Class(found_error)]
            if module.types.get(*arguments as usize) == Some(&BcType::Str)
                && *found_option == option
                && matches!(option_args.as_slice(), [value]
                    if module.types.get(*value as usize) == Some(&BcType::Str))
                && *found_env == environment
                && *found_input == input
                && *found_output == output
                && *found_error == output);
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !valid
        {
            return Err(terr(
                "the ExecSpec role does not name its final value class".to_string(),
            ));
        }
    }
    if let Some(size) = slot(lm_bytecode::corepin::ROLE_TTY_SIZE) {
        let class = &module.classes[size as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || !class.is_frozen
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !matches!(fields.as_slice(), [BcType::Int, BcType::Int])
        {
            return Err(terr(
                "the TtySize role does not name its frozen value class".to_string(),
            ));
        }
    }
    if let Some(info) = slot(ROLE_FILE_INFO) {
        let Some(kind) = slot(ROLE_FILE_KIND) else {
            return Err(terr(
                "the FileInfo role requires the FileKind role".to_string(),
            ));
        };
        let Some(option) = slot(ROLE_OPTION) else {
            return Err(terr(
                "the FileInfo role requires the Option role".to_string(),
            ));
        };
        let class = &module.classes[info as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        let valid_fields = matches!(fields.as_slice(), [BcType::Class(found_kind), BcType::Int, BcType::Inst(found_option, args), BcType::Bool]
            if *found_kind == kind
                && *found_option == option
                && matches!(args.as_slice(), [item]
                    if module.types.get(*item as usize) == Some(&BcType::Int)));
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !valid_fields
        {
            return Err(terr(
                "the FileInfo role does not name its final value class".to_string(),
            ));
        }
    }
    if let Some(entry) = slot(ROLE_DIR_ENTRY) {
        let Some(kind) = slot(ROLE_FILE_KIND) else {
            return Err(terr(
                "the DirEntry role requires the FileKind role".to_string(),
            ));
        };
        let class = &module.classes[entry as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !matches!(fields.as_slice(), [BcType::Str, BcType::Class(found)] if *found == kind)
        {
            return Err(terr(
                "the DirEntry role does not name its final value class".to_string(),
            ));
        }
    }
    for (role, name, arity) in [
        (lm_bytecode::corepin::ROLE_LIST, "List", 1),
        (lm_bytecode::corepin::ROLE_MAP, "Map", 2),
        (lm_bytecode::corepin::ROLE_FUNCTION_DEF, "FunctionDef", 2),
        (lm_bytecode::corepin::ROLE_FUNCTION_CODE, "FunctionCode", 2),
    ] {
        let Some(idx) = slot(role) else { continue };
        let class = &module.classes[idx as usize];
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != arity
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !class.fields.is_empty()
        {
            return Err(terr(format!(
                "the core role `{name}` does not name its native collection class"
            )));
        }
    }
    if let Some(definition) = slot(lm_bytecode::corepin::ROLE_DEFINITION_SOURCE) {
        let class = &module.classes[definition as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        let syntax = slot(lm_bytecode::corepin::ROLE_SYNTAX_NODE);
        let spec = slot(lm_bytecode::corepin::ROLE_DEFINITION_SPEC);
        let valid_fields = matches!(fields.as_slice(), [BcType::Str, BcType::Class(found), BcType::Class(definition)]
            if Some(*found) == syntax
                && Some(*definition) == spec);
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !valid_fields
        {
            return Err(terr(
                "the DefinitionSource role has an invalid layout".to_string(),
            ));
        }
    }
    if let Some(definition) = slot(lm_bytecode::corepin::ROLE_DEFINITION_SPEC) {
        let class = &module.classes[definition as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        let slot_spec = slot(lm_bytecode::corepin::ROLE_SLOT_SPEC);
        let identity = slot(lm_bytecode::corepin::ROLE_DEFINITION_IDENTITY);
        let valid_fields = matches!(fields.as_slice(), [BcType::Class(found), BcType::Digest, BcType::List(item)]
            if Some(*found) == identity
                && matches!(module.types.get(*item as usize), Some(BcType::Class(found)) if Some(*found) == slot_spec));
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !valid_fields
        {
            return Err(terr(
                "the DefinitionSpec role has an invalid layout".to_string(),
            ));
        }
    }
    if let Some(definition) = slot(lm_bytecode::corepin::ROLE_DEFINITION_IDENTITY) {
        let class = &module.classes[definition as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !matches!(
                fields.as_slice(),
                [BcType::Str, BcType::Str, BcType::Digest, BcType::Digest]
            )
        {
            return Err(terr(
                "the DefinitionIdentity role has an invalid layout".to_string(),
            ));
        }
    }
    if let Some(range) = slot(lm_bytecode::corepin::ROLE_SOURCE_RANGE) {
        let class = &module.classes[range as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !matches!(fields.as_slice(), [BcType::Int, BcType::Int])
        {
            return Err(terr(
                "the SourceRange role has an invalid layout".to_string(),
            ));
        }
    }
    if let Some(location) = slot(lm_bytecode::corepin::ROLE_CODE_LOCATION) {
        let Some(range) = slot(lm_bytecode::corepin::ROLE_SOURCE_RANGE) else {
            return Err(terr(
                "the CodeLocation role requires the SourceRange role".to_string(),
            ));
        };
        let class = &module.classes[location as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        let option = slot(ROLE_OPTION);
        let valid_fields = matches!(fields.as_slice(), [BcType::Inst(path_option, path_args), BcType::Inst(range_option, range_args), BcType::Digest, BcType::Int]
            if Some(*path_option) == option
                && Some(*range_option) == option
                && matches!(path_args.as_slice(), [path]
                    if matches!(module.types.get(*path as usize), Some(BcType::Str)))
                && matches!(range_args.as_slice(), [item]
                    if matches!(module.types.get(*item as usize), Some(BcType::Class(found)) if *found == range)));
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !valid_fields
        {
            return Err(terr(
                "the CodeLocation role has an invalid layout".to_string(),
            ));
        }
    }
    for (role, name) in [
        (lm_bytecode::corepin::ROLE_CODE_ERROR, "CodeError"),
        (lm_bytecode::corepin::ROLE_COMPILE_ERRORS, "CompileErrors"),
    ] {
        let Some(idx) = slot(role) else { continue };
        let class = &module.classes[idx as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || fields.len() != 1
            || fields[0] != &BcType::Str
        {
            return Err(terr(format!(
                "the {name} role does not name its final error class"
            )));
        }
    }
    if let Some(idx) = slot(lm_bytecode::corepin::ROLE_COMPILE_ENV) {
        let Some(verified) = slot(lm_bytecode::corepin::ROLE_VERIFIED_MODULE) else {
            return Err(terr(
                "the CompileEnv role requires the VerifiedModule role".to_string(),
            ));
        };
        let class = &module.classes[idx as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        let modules_ok = fields.first().is_some_and(|field| match field {
            BcType::List(element) => {
                module.types.get(*element as usize) == Some(&BcType::Class(verified))
            }
            _ => false,
        });
        let roots_ok = fields.get(1).is_some_and(|field| match field {
            BcType::List(element) => match module.types.get(*element as usize) {
                Some(BcType::Tuple(parts)) if parts.len() == 2 => parts
                    .iter()
                    .all(|part| module.types.get(*part as usize) == Some(&BcType::Str)),
                _ => false,
            },
            _ => false,
        });
        let definition = slot(lm_bytecode::corepin::ROLE_DEFINITION_SPEC);
        let definitions_ok = fields.get(2).is_some_and(|field| match field {
            BcType::List(element) => match module.types.get(*element as usize) {
                Some(BcType::Tuple(parts)) if parts.len() == 2 => {
                    module.types.get(parts[0] as usize) == Some(&BcType::Str)
                        && matches!(module.types.get(parts[1] as usize), Some(BcType::Class(found)) if Some(*found) == definition)
                }
                _ => false,
            },
            _ => false,
        });
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || fields.len() != 3
            || !modules_ok
            || !roots_ok
            || !definitions_ok
        {
            return Err(terr(
                "the CompileEnv role does not name its final environment class".to_string(),
            ));
        }
    }
    if let Some(idx) = slot(lm_bytecode::corepin::ROLE_LINK_ENV) {
        let Some(instance) = slot(lm_bytecode::corepin::ROLE_INSTANCE) else {
            return Err(terr(
                "the LinkEnv role requires the Instance role".to_string(),
            ));
        };
        let class = &module.classes[idx as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        let instances_ok = fields.first().is_some_and(|field| match field {
            BcType::List(element) => {
                module.types.get(*element as usize) == Some(&BcType::Class(instance))
            }
            _ => false,
        });
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || fields.len() != 1
            || !instances_ok
        {
            return Err(terr(
                "the LinkEnv role does not name its final environment class".to_string(),
            ));
        }
    }
    if let Some(idx) = slot(lm_bytecode::corepin::ROLE_COMPILE_OPTIONS) {
        let class = &module.classes[idx as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        let string_list = |field: Option<&&BcType>| {
            field.is_some_and(|field| match field {
                BcType::List(element) => module.types.get(*element as usize) == Some(&BcType::Str),
                _ => false,
            })
        };
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || fields.len() != 5
            || fields[0] != &BcType::Bool
            || fields[1] != &BcType::Bool
            || fields[2] != &BcType::Bool
            || !string_list(fields.get(3))
            || !string_list(fields.get(4))
        {
            return Err(terr(
                "the CompileOptions role does not name its final options class".to_string(),
            ));
        }
    }
    if let Some(idx) = slot(lm_bytecode::corepin::ROLE_DYN_VALUE) {
        let class = &module.classes[idx as usize];
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !class.fields.is_empty()
        {
            return Err(terr(
                "the DynValue role does not name its final native class".to_string(),
            ));
        }
    }
    let syntax_roles = [
        slot(lm_bytecode::corepin::ROLE_SYNTAX_TREE),
        slot(lm_bytecode::corepin::ROLE_SYNTAX_ELEMENT),
        slot(lm_bytecode::corepin::ROLE_SYNTAX_NODE),
        slot(lm_bytecode::corepin::ROLE_SYNTAX_TOKEN),
        slot(lm_bytecode::corepin::ROLE_SYNTAX_TRIVIA),
        slot(lm_bytecode::corepin::ROLE_SYNTAX_BUILDER),
        slot(lm_bytecode::corepin::ROLE_PARSE_STATUS),
        slot(lm_bytecode::corepin::ROLE_SYNTAX_DIAGNOSTIC),
        slot(lm_bytecode::corepin::ROLE_SYNTAX_PARSE),
    ];
    if syntax_roles.iter().any(Option::is_some) && syntax_roles.iter().any(Option::is_none) {
        return Err(terr(
            "the syntax family resolves without every value class".to_string(),
        ));
    }
    if let [Some(tree), Some(element), Some(node), Some(token), Some(trivia), Some(builder), Some(status), Some(diagnostic), Some(parse)] =
        syntax_roles
    {
        let field_types = |class: u32| -> Vec<&BcType> {
            module.classes[class as usize]
                .fields
                .iter()
                .filter_map(|(_, ty)| module.types.get(*ty as usize))
                .collect()
        };
        let tree_class = &module.classes[tree as usize];
        let tree_fields = field_types(tree);
        if tree_class.kind != BcClassKind::Normal
            || !tree_class.is_final
            || tree_class.type_params != 0
            || tree_class.parent().is_some()
            || tree_fields != [&BcType::Str, &BcType::Bytes]
        {
            return Err(terr(
                "the SyntaxTree role does not name its final value class".to_string(),
            ));
        }
        let view_fields = [&BcType::Str, &BcType::Bytes, &BcType::Int];
        let element_class = &module.classes[element as usize];
        if element_class.kind != BcClassKind::Normal
            || element_class.is_final
            || element_class.type_params != 0
            || element_class.parent().is_some()
            || field_types(element) != view_fields
        {
            return Err(terr(
                "the SyntaxElement role does not name its base value class".to_string(),
            ));
        }
        let node_class = &module.classes[node as usize];
        if node_class.kind != BcClassKind::Normal
            || node_class.is_final
            || node_class.type_params != 0
            || node_class.parent() != Some(element)
            || field_types(node) != view_fields
        {
            return Err(terr(
                "the SyntaxNode role does not name its node value class".to_string(),
            ));
        }
        for (class, name) in [(token, "SyntaxToken"), (trivia, "SyntaxTrivia")] {
            let info = &module.classes[class as usize];
            if info.kind != BcClassKind::Normal
                || !info.is_final
                || info.type_params != 0
                || info.parent() != Some(element)
                || field_types(class) != view_fields
            {
                return Err(terr(format!(
                    "the {name} role does not name its final view class"
                )));
            }
        }
        let builder_class = &module.classes[builder as usize];
        if builder_class.kind != BcClassKind::Normal
            || !builder_class.is_final
            || builder_class.type_params != 0
            || builder_class.parent().is_some()
            || !builder_class.fields.is_empty()
        {
            return Err(terr(
                "the SyntaxBuilder role does not name its final stateless class".to_string(),
            ));
        }
        let diagnostic_class = &module.classes[diagnostic as usize];
        let diagnostic_fields = field_types(diagnostic);
        if diagnostic_class.kind != BcClassKind::Normal
            || !diagnostic_class.is_final
            || diagnostic_class.type_params != 0
            || diagnostic_class.parent().is_some()
            || diagnostic_fields != [&BcType::Int, &BcType::Int, &BcType::Str]
        {
            return Err(terr(
                "the SyntaxDiagnostic role does not name its final value class".to_string(),
            ));
        }
        let parse_class = &module.classes[parse as usize];
        let parse_fields = field_types(parse);
        let diagnostic_list = parse_fields.get(2).is_some_and(|field| match field {
            BcType::List(element) => {
                module.types.get(*element as usize) == Some(&BcType::Class(diagnostic))
            }
            _ => false,
        });
        if parse_class.kind != BcClassKind::Normal
            || !parse_class.is_final
            || parse_class.type_params != 0
            || parse_class.parent().is_some()
            || parse_fields.len() != 3
            || parse_fields.first() != Some(&&BcType::Class(tree))
            || parse_fields.get(1) != Some(&&BcType::Class(status))
            || !diagnostic_list
        {
            return Err(terr(
                "the SyntaxParse role does not name its final result class".to_string(),
            ));
        }
    }
    let text_roles = [
        slot(lm_bytecode::corepin::ROLE_TEXT),
        slot(lm_bytecode::corepin::ROLE_STRING),
        slot(lm_bytecode::corepin::ROLE_SUBSTRING),
    ];
    if text_roles.iter().any(Option::is_some) && text_roles.iter().any(Option::is_none) {
        return Err(terr(
            "the Text family resolves without every concrete class".to_string(),
        ));
    }
    if let [Some(text), Some(string), Some(substring)] = text_roles {
        let class = &module.classes[text as usize];
        if class.kind != BcClassKind::Abstract
            || class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !class.fields.is_empty()
        {
            return Err(terr(
                "the core role `Text` does not name its abstract stateless parent".to_string(),
            ));
        }
        for (idx, name) in [(string, "String"), (substring, "Substring")] {
            let class = &module.classes[idx as usize];
            if class.kind != BcClassKind::Normal
                || !class.is_final
                || class.type_params != 0
                || class.parent() != Some(text)
                || !class.parent_args.is_empty()
                || !class.fields.is_empty()
            {
                return Err(terr(format!(
                    "the core role `{name}` does not name a final stateless Text class"
                )));
            }
        }
        for (idx, class) in module.classes.iter().enumerate() {
            if class.parent() == Some(text) && idx as u32 != string && idx as u32 != substring {
                return Err(terr(
                    "a class other than String or Substring extends Text".to_string(),
                ));
            }
        }
    }
    Ok(())
}
