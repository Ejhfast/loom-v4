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
pub(crate) const ROLE_RUN_RESULT: usize = 8;
pub(crate) const ROLE_RUN_DONE: usize = 9;
pub(crate) const ROLE_RUN_FAULT: usize = 10;
pub(crate) const ROLE_STEP_EVENT: usize = 11;
pub(crate) const ROLE_STEP_RAN: usize = 12;
pub(crate) const ROLE_STEP_WAITING: usize = 13;
pub(crate) const ROLE_STEP_DONE: usize = 14;
pub(crate) const ROLE_STEP_FAULT: usize = 15;
pub(crate) const ROLE_DRIVE_EVENT: usize = 16;
pub(crate) const ROLE_DRIVE_ASKED: usize = 17;
pub(crate) const ROLE_DRIVE_DONE: usize = 18;
pub(crate) const ROLE_DRIVE_FAULT: usize = 19;
pub(crate) const ROLE_RECV: usize = 20;
pub(crate) const ROLE_RECV_MSG: usize = 21;
pub(crate) const ROLE_RECV_CLOSED: usize = 22;
pub(crate) const ROLE_SEND_RESULT: usize = 23;
pub(crate) const ROLE_SEND_SENT: usize = 24;
pub(crate) const ROLE_SEND_CLOSED: usize = 25;
pub(crate) const ROLE_SEND_FAULT: usize = 26;
pub(crate) const ROLE_PROC_RESULT: usize = 27;
pub(crate) const ROLE_PROC_DONE: usize = 28;
pub(crate) const ROLE_PROC_FAULT: usize = 29;
pub(crate) const ROLE_PROC_ERROR: usize = 30;
pub(crate) const ROLE_PROC_ERROR_DEAD: usize = 31;
pub(crate) const ROLE_PROC_ERROR_NOT_PAUSED: usize = 32;
pub(crate) const ROLE_PROC_ERROR_ALREADY_PAUSED: usize = 33;
pub(crate) const ROLE_PROC_ERROR_IN_USE: usize = 34;
pub(crate) const ROLE_PROC_CLASS: usize = 35;
pub(crate) const ROLE_SNAPSHOT_ERROR: usize = 36;
pub(crate) const ROLE_SNAPSHOT_RESOURCE_ACTIVE: usize = 37;
pub(crate) const ROLE_SNAPSHOT_LIMIT_EXCEEDED: usize = 38;
pub(crate) const ROLE_SNAPSHOT_BAD_IMAGE: usize = 39;
pub(crate) const ROLE_RESTORE_ERROR: usize = 40;
pub(crate) const ROLE_RESTORE_LIMIT_EXCEEDED: usize = 41;
pub(crate) const ROLE_FS_ERROR: usize = 42;
pub(crate) const ROLE_FS_ERROR_CLOSED: usize = 43;
pub(crate) const ROLE_FS_ERROR_FAILED: usize = 44;
pub(crate) const ROLE_OPEN_OPTIONS: usize = 45;
pub(crate) const ROLE_OPEN_READ_ONLY: usize = 46;
pub(crate) const ROLE_OPEN_WRITE_ONLY: usize = 47;
pub(crate) const ROLE_OPEN_READ_WRITE: usize = 48;
pub(crate) const ROLE_OPEN_CREATE: usize = 49;
pub(crate) const ROLE_OPEN_CREATE_TRUNCATE: usize = 50;
pub(crate) const ROLE_OPEN_APPEND: usize = 51;
pub(crate) const ROLE_SEEK_FROM: usize = 52;
pub(crate) const ROLE_SEEK_START: usize = 53;
pub(crate) const ROLE_SEEK_CURRENT: usize = 54;
pub(crate) const ROLE_SEEK_END: usize = 55;
pub(crate) const ROLE_PAIR: usize = 68;
pub(crate) const ROLE_IP_ADDRESS: usize = 69;
pub(crate) const ROLE_IP_V4: usize = 70;
pub(crate) const ROLE_IP_V6: usize = 71;
pub(crate) const ROLE_SOCKET_ADDRESS: usize = 72;
pub(crate) const ROLE_NET_ERROR: usize = 73;
pub(crate) const ROLE_NET_INVALID_INPUT: usize = 74;
pub(crate) const ROLE_NET_NAME_NOT_FOUND: usize = 75;
pub(crate) const ROLE_NET_UNAVAILABLE: usize = 76;
pub(crate) const ROLE_NET_PERMISSION_DENIED: usize = 77;
pub(crate) const ROLE_NET_ADDRESS_IN_USE: usize = 78;
pub(crate) const ROLE_NET_CONNECTION_REFUSED: usize = 79;
pub(crate) const ROLE_NET_CONNECTION_RESET: usize = 80;
pub(crate) const ROLE_NET_NOT_CONNECTED: usize = 81;
pub(crate) const ROLE_NET_TIMED_OUT: usize = 82;
pub(crate) const ROLE_NET_CLOSED: usize = 83;
pub(crate) const ROLE_NET_LIMIT_EXCEEDED: usize = 84;
pub(crate) const ROLE_NET_UNSUPPORTED: usize = 85;
pub(crate) const ROLE_NET_FAILED: usize = 86;
pub(crate) const ROLE_TCP_READ: usize = 87;
pub(crate) const ROLE_TCP_READ_DATA: usize = 88;
pub(crate) const ROLE_TCP_READ_END: usize = 89;
pub(crate) const ROLE_SHUTDOWN: usize = 90;
pub(crate) const ROLE_SHUTDOWN_READ: usize = 91;
pub(crate) const ROLE_SHUTDOWN_WRITE: usize = 92;
pub(crate) const ROLE_SHUTDOWN_BOTH: usize = 93;
pub(crate) const ROLE_TCP_RESOURCE: usize = 94;
pub(crate) const ROLE_TCP_STREAM: usize = 95;
pub(crate) const ROLE_TCP_LISTENER: usize = 96;
pub(crate) const ROLE_TLS_ERROR: usize = 97;
pub(crate) const ROLE_TLS_INVALID_CONFIG: usize = 98;
pub(crate) const ROLE_TLS_HANDSHAKE: usize = 99;
pub(crate) const ROLE_TLS_CERTIFICATE: usize = 100;
pub(crate) const ROLE_TLS_PROTOCOL: usize = 101;
pub(crate) const ROLE_TLS_NETWORK: usize = 102;
pub(crate) const ROLE_TLS_CLOSED: usize = 103;
pub(crate) const ROLE_TLS_LIMIT_EXCEEDED: usize = 104;
pub(crate) const ROLE_PARSE_STATUS: usize = 125;
pub(crate) const ROLE_PARSE_COMPLETE: usize = 126;
pub(crate) const ROLE_PARSE_INCOMPLETE: usize = 127;
pub(crate) const ROLE_PARSE_INVALID: usize = 128;

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
}

/// One core family: the parent role, the generic arity, and the arm
/// roles in declaration order.
const CORE_FAMILIES: [(usize, u32, &[usize], &str); 21] = [
    (
        ROLE_OPTION,
        1,
        &[ROLE_OPTION_SOME, ROLE_OPTION_NONE],
        "Option",
    ),
    (ROLE_RESULT, 2, &[ROLE_RESULT_OK, ROLE_RESULT_ERR], "Result"),
    (ROLE_IO_ERROR, 0, &[ROLE_IO_ERROR_FAILED], "IoError"),
    (
        ROLE_RUN_RESULT,
        1,
        &[ROLE_RUN_DONE, ROLE_RUN_FAULT],
        "RunResult",
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
        ROLE_PROC_RESULT,
        1,
        &[ROLE_PROC_DONE, ROLE_PROC_FAULT],
        "ProcResult",
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
        ROLE_FS_ERROR,
        0,
        &[ROLE_FS_ERROR_CLOSED, ROLE_FS_ERROR_FAILED],
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
];

/// The field layout every core arm must carry, by role.
const CORE_ARM_FIELDS: [(usize, &[FieldShape]); 70] = [
    (ROLE_OPTION_SOME, &[FieldShape::Var(0)]),
    (ROLE_OPTION_NONE, &[]),
    (ROLE_RESULT_OK, &[FieldShape::Var(0)]),
    (ROLE_RESULT_ERR, &[FieldShape::Var(1)]),
    (ROLE_IO_ERROR_FAILED, &[FieldShape::Str]),
    (ROLE_RUN_DONE, &[FieldShape::Var(0)]),
    (ROLE_RUN_FAULT, &[FieldShape::Fault]),
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
    (ROLE_PROC_DONE, &[FieldShape::Var(0)]),
    (ROLE_PROC_FAULT, &[FieldShape::Fault]),
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
    (ROLE_FS_ERROR_CLOSED, &[]),
    (ROLE_FS_ERROR_FAILED, &[FieldShape::Str]),
    (ROLE_OPEN_READ_ONLY, &[]),
    (ROLE_OPEN_WRITE_ONLY, &[]),
    (ROLE_OPEN_READ_WRITE, &[]),
    (ROLE_OPEN_CREATE, &[]),
    (ROLE_OPEN_CREATE_TRUNCATE, &[]),
    (ROLE_OPEN_APPEND, &[]),
    (ROLE_SEEK_START, &[FieldShape::Int]),
    (ROLE_SEEK_CURRENT, &[FieldShape::Int]),
    (ROLE_SEEK_END, &[FieldShape::Int]),
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
    let mut taken: Vec<u32> = Vec::new();
    for role in 0..lm_bytecode::CORE_ROLE_COUNT {
        let Some(idx) = slot(role) else { continue };
        if idx as usize >= module.classes.len() {
            return Err(terr(format!(
                "core role {role} names a class outside the table"
            )));
        }
        if taken.contains(&idx) {
            return Err(terr(format!(
                "core role {role} names a class another role took"
            )));
        }
        taken.push(idx);
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
                let ok = match want {
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
    if let Some(idx) = slot(ROLE_PAIR) {
        let class = &module.classes[idx as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        if class.kind != BcClassKind::Normal
            || class.is_final
            || class.type_params != 2
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || fields.len() != 2
            || fields[0] != &BcType::Var(0)
            || fields[1] != &BcType::Var(1)
        {
            return Err(terr(
                "the core role `Pair` does not name its two-field generic class".to_string(),
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
    let native_roles = [
        (lm_bytecode::corepin::ROLE_INT, "Int"),
        (lm_bytecode::corepin::ROLE_BOOL, "Bool"),
        (lm_bytecode::corepin::ROLE_BYTES, "Bytes"),
        (lm_bytecode::corepin::ROLE_STRING_BUILDER, "StringBuilder"),
        (lm_bytecode::corepin::ROLE_BYTE_BUFFER, "ByteBuffer"),
        (lm_bytecode::corepin::ROLE_CHAR, "Char"),
        (lm_bytecode::corepin::ROLE_TLS_STREAM, "TlsStream"),
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
        let slot_spec = slot(lm_bytecode::corepin::ROLE_SLOT_SPEC);
        let valid_fields = matches!(fields.as_slice(), [BcType::Str, BcType::Class(found), BcType::Digest, BcType::List(item)]
            if Some(*found) == syntax
                && matches!(module.types.get(*item as usize), Some(BcType::Class(found)) if Some(*found) == slot_spec));
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
        let valid_fields = matches!(fields.as_slice(), [BcType::Str, BcType::Str, BcType::Digest, BcType::List(item)]
            if matches!(module.types.get(*item as usize), Some(BcType::Class(found)) if Some(*found) == slot_spec));
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
