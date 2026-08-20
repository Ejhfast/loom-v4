//! Canonical operation, group, and intrinsic manifests.
//!
//! This crate is the one source for effect groups, exact operations,
//! their signatures, and their stable identities. The checker, the
//! verifier, the VM, and the host all read this table. Nothing else
//! defines an operation.
//!
//! An exact operation has a dense slot: its index in `OPS`. The slot
//! order is part of the manifest identity. `manifest_digest` pins the
//! full manifest: version, groups, names, signatures, and order. A
//! change to any of these changes the digest and therefore the ABI
//! version.

mod fault;
mod sha;

pub use fault::{FaultCode, SnapshotClass, FAULT_CODES};
pub use sha::{sha256, sha256_hex};

/// The manifest ABI version. A signature or membership change must
/// increment this value.
///
/// Version 2 adds the eight proc operations of specification 23.6.
/// Version 3 adds the four snapshot operations of specification 23.5.
/// Version 4 hashes every field of one operation definition into its
/// identity. Version 5 adds immutable bytes, file handles, and the
/// first six filesystem operations. Version 6 adds holder resource
/// controls and fuel-bounded snapshot waiting. Version 7 adds typed
/// waits and selectable drive and receive sources. Version 8 adds
/// transparent effect sets and the DNS and TCP operations. Version 9
/// adds the TLS client resource and its effect sets.
pub const ABI_VERSION: u32 = 10;

/// A dense group slot: the index in `GROUPS`.
pub type GroupSlot = u32;

/// A dense operation slot: the index in `OPS`.
pub type OpSlot = u32;

/// The effect groups and effect sets, in canonical slot order.
///
/// The first ten entries keep their existing slots. An empty member
/// list makes a namespace group. Its members are the operations whose
/// `group` field names that group. A nonempty list can name exact
/// operations or other groups.
pub const GROUPS: [&str; 22] = [
    "Io",
    "Fs",
    "Clock",
    "Rand",
    "Net",
    "Proc",
    "Vm",
    "Compiler",
    "Reflect",
    "Wait",
    "Dns",
    "Tcp",
    "Tcp.Stream",
    "Tcp.Listener",
    "Tcp.Client",
    "Tcp.Server",
    "Http.CleartextClient",
    "Tls",
    "Tls.Stream",
    "Tls.Client",
    "Http.Client",
    "Choose",
];

const TCP_STREAM_MEMBERS: &[&str] = &[
    "Tcp.Read",
    "Tcp.Write",
    "Tcp.Shutdown",
    "Tcp.LocalAddress",
    "Tcp.PeerAddress",
    "Tcp.Close",
];
const TCP_LISTENER_MEMBERS: &[&str] =
    &["Tcp.Listen", "Tcp.Accept", "Tcp.LocalAddress", "Tcp.Close"];
const TCP_CLIENT_MEMBERS: &[&str] = &["Tcp.Connect", "Tcp.Stream"];
const TCP_SERVER_MEMBERS: &[&str] = &["Tcp.Listener", "Tcp.Stream"];
const HTTP_CLEARTEXT_MEMBERS: &[&str] = &["Dns.Resolve", "Tcp.Client"];
const TLS_STREAM_MEMBERS: &[&str] = &[
    "Tls.Read",
    "Tls.Write",
    "Tls.Shutdown",
    "Tls.LocalAddress",
    "Tls.PeerAddress",
    "Tls.Close",
];
const TLS_CLIENT_MEMBERS: &[&str] = &["Tls.Handshake", "Tls.Stream"];
const HTTP_CLIENT_MEMBERS: &[&str] = &["Dns.Resolve", "Tcp.Client", "Tls.Client"];

/// The explicit members of each group slot.
pub const GROUP_MEMBERS: [&[&str]; 22] = [
    &[],
    &[],
    &[],
    &[],
    &[],
    &[],
    &[],
    &[],
    &[],
    &[],
    &[],
    &[],
    TCP_STREAM_MEMBERS,
    TCP_LISTENER_MEMBERS,
    TCP_CLIENT_MEMBERS,
    TCP_SERVER_MEMBERS,
    HTTP_CLEARTEXT_MEMBERS,
    &[],
    TLS_STREAM_MEMBERS,
    TLS_CLIENT_MEMBERS,
    HTTP_CLIENT_MEMBERS,
    &[],
];

/// One primitive manifest type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiPrimitive {
    Unit,
    Bool,
    Int,
    String,
    Bytes,
    SnapshotImage,
}

impl AbiPrimitive {
    fn text(self) -> &'static str {
        match self {
            AbiPrimitive::Unit => "()",
            AbiPrimitive::Bool => "Bool",
            AbiPrimitive::Int => "Int",
            AbiPrimitive::String => "String",
            AbiPrimitive::Bytes => "Bytes",
            AbiPrimitive::SnapshotImage => "SnapshotImage",
        }
    }
}

/// One core-image class used by a manifest type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiCore {
    Text,
    Substring,
    Char,
    StringBuilder,
    ByteBuffer,
    OpenOptions,
    SeekFrom,
    IoError,
    FsError,
    SnapshotError,
    IpAddress,
    SocketAddress,
    NetError,
    TcpRead,
    Shutdown,
    TlsError,
}

impl AbiCore {
    fn text(self) -> &'static str {
        match self {
            AbiCore::Text => "Text",
            AbiCore::Substring => "Substring",
            AbiCore::Char => "Char",
            AbiCore::StringBuilder => "StringBuilder",
            AbiCore::ByteBuffer => "ByteBuffer",
            AbiCore::OpenOptions => "OpenOptions",
            AbiCore::SeekFrom => "SeekFrom",
            AbiCore::IoError => "IoError",
            AbiCore::FsError => "FsError",
            AbiCore::SnapshotError => "SnapshotError",
            AbiCore::IpAddress => "IpAddress",
            AbiCore::SocketAddress => "SocketAddress",
            AbiCore::NetError => "NetError",
            AbiCore::TcpRead => "TcpRead",
            AbiCore::Shutdown => "Shutdown",
            AbiCore::TlsError => "TlsError",
        }
    }
}

/// One native resource type used by the operation manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiNative {
    FileHandle,
    TcpResource,
    TcpStream,
    TcpListener,
    TlsStream,
}

impl AbiNative {
    fn text(self) -> &'static str {
        match self {
            AbiNative::FileHandle => "FileHandle",
            AbiNative::TcpResource => "TcpResource",
            AbiNative::TcpStream => "TcpStream",
            AbiNative::TcpListener => "TcpListener",
            AbiNative::TlsStream => "TlsStream",
        }
    }
}

/// One generic core family used by a manifest type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiConstructor {
    Option,
    Result,
    Pair,
}

impl AbiConstructor {
    pub fn text(self) -> &'static str {
        match self {
            AbiConstructor::Option => "Option",
            AbiConstructor::Result => "Result",
            AbiConstructor::Pair => "Pair",
        }
    }

    pub fn arity(self) -> usize {
        match self {
            AbiConstructor::Option => 1,
            AbiConstructor::Result => 2,
            AbiConstructor::Pair => 2,
        }
    }
}

/// One normalized manifest type expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiType {
    Primitive(AbiPrimitive),
    Core(AbiCore),
    Native(AbiNative),
    /// One generic type parameter of an intrinsic declaration.
    Var(u32),
    List(&'static AbiType),
    Map(&'static AbiType, &'static AbiType),
    Tuple(&'static [AbiType]),
    Apply(AbiConstructor, &'static [AbiType]),
}

impl AbiType {
    pub const UNIT: AbiType = AbiType::Primitive(AbiPrimitive::Unit);
    pub const BOOL: AbiType = AbiType::Primitive(AbiPrimitive::Bool);
    pub const INT: AbiType = AbiType::Primitive(AbiPrimitive::Int);
    pub const STR: AbiType = AbiType::Primitive(AbiPrimitive::String);
    pub const BYTES: AbiType = AbiType::Primitive(AbiPrimitive::Bytes);
    pub const SNAPSHOT_IMAGE: AbiType = AbiType::Primitive(AbiPrimitive::SnapshotImage);
    pub const TEXT: AbiType = AbiType::Core(AbiCore::Text);
    pub const SUBSTRING: AbiType = AbiType::Core(AbiCore::Substring);
    pub const CHAR: AbiType = AbiType::Core(AbiCore::Char);
    pub const STRING_BUILDER: AbiType = AbiType::Core(AbiCore::StringBuilder);
    pub const BYTE_BUFFER: AbiType = AbiType::Core(AbiCore::ByteBuffer);
    pub const OPEN_OPTIONS: AbiType = AbiType::Core(AbiCore::OpenOptions);
    pub const SEEK_FROM: AbiType = AbiType::Core(AbiCore::SeekFrom);
    pub const IO_ERROR: AbiType = AbiType::Core(AbiCore::IoError);
    pub const FS_ERROR: AbiType = AbiType::Core(AbiCore::FsError);
    pub const SNAPSHOT_ERROR: AbiType = AbiType::Core(AbiCore::SnapshotError);
    pub const IP_ADDRESS: AbiType = AbiType::Core(AbiCore::IpAddress);
    pub const SOCKET_ADDRESS: AbiType = AbiType::Core(AbiCore::SocketAddress);
    pub const NET_ERROR: AbiType = AbiType::Core(AbiCore::NetError);
    pub const TCP_READ: AbiType = AbiType::Core(AbiCore::TcpRead);
    pub const SHUTDOWN: AbiType = AbiType::Core(AbiCore::Shutdown);
    pub const TLS_ERROR: AbiType = AbiType::Core(AbiCore::TlsError);
    pub const FILE_HANDLE: AbiType = AbiType::Native(AbiNative::FileHandle);
    pub const TCP_RESOURCE: AbiType = AbiType::Native(AbiNative::TcpResource);
    pub const TCP_STREAM: AbiType = AbiType::Native(AbiNative::TcpStream);
    pub const TCP_LISTENER: AbiType = AbiType::Native(AbiNative::TcpListener);
    pub const TLS_STREAM: AbiType = AbiType::Native(AbiNative::TlsStream);

    pub const LIST_SUBSTRING: AbiType = AbiType::List(&AbiType::SUBSTRING);
    pub const RESULT_OPTION_STR_IO_ERROR: AbiType = AbiType::Apply(
        AbiConstructor::Result,
        &[
            AbiType::Apply(AbiConstructor::Option, &[AbiType::STR]),
            AbiType::IO_ERROR,
        ],
    );
    pub const RESULT_SNAPSHOT_IMAGE_ERROR: AbiType = AbiType::Apply(
        AbiConstructor::Result,
        &[AbiType::SNAPSHOT_IMAGE, AbiType::SNAPSHOT_ERROR],
    );
    pub const RESULT_FILE_HANDLE_FS_ERROR: AbiType = AbiType::Apply(
        AbiConstructor::Result,
        &[AbiType::FILE_HANDLE, AbiType::FS_ERROR],
    );
    pub const RESULT_BYTES_FS_ERROR: AbiType =
        AbiType::Apply(AbiConstructor::Result, &[AbiType::BYTES, AbiType::FS_ERROR]);
    pub const RESULT_INT_FS_ERROR: AbiType =
        AbiType::Apply(AbiConstructor::Result, &[AbiType::INT, AbiType::FS_ERROR]);
    pub const RESULT_UNIT_FS_ERROR: AbiType =
        AbiType::Apply(AbiConstructor::Result, &[AbiType::UNIT, AbiType::FS_ERROR]);
    pub const LIST_SOCKET_ADDRESS: AbiType = AbiType::List(&AbiType::SOCKET_ADDRESS);
    pub const LIST_BYTES: AbiType = AbiType::List(&AbiType::BYTES);
    pub const RESULT_LIST_SOCKET_ADDRESS_NET_ERROR: AbiType = AbiType::Apply(
        AbiConstructor::Result,
        &[AbiType::LIST_SOCKET_ADDRESS, AbiType::NET_ERROR],
    );
    pub const RESULT_TCP_STREAM_NET_ERROR: AbiType = AbiType::Apply(
        AbiConstructor::Result,
        &[AbiType::TCP_STREAM, AbiType::NET_ERROR],
    );
    pub const RESULT_TCP_LISTENER_NET_ERROR: AbiType = AbiType::Apply(
        AbiConstructor::Result,
        &[AbiType::TCP_LISTENER, AbiType::NET_ERROR],
    );
    pub const PAIR_TCP_STREAM_SOCKET_ADDRESS: AbiType = AbiType::Apply(
        AbiConstructor::Pair,
        &[AbiType::TCP_STREAM, AbiType::SOCKET_ADDRESS],
    );
    pub const RESULT_ACCEPT_NET_ERROR: AbiType = AbiType::Apply(
        AbiConstructor::Result,
        &[AbiType::PAIR_TCP_STREAM_SOCKET_ADDRESS, AbiType::NET_ERROR],
    );
    pub const RESULT_TCP_READ_NET_ERROR: AbiType = AbiType::Apply(
        AbiConstructor::Result,
        &[AbiType::TCP_READ, AbiType::NET_ERROR],
    );
    pub const RESULT_INT_NET_ERROR: AbiType =
        AbiType::Apply(AbiConstructor::Result, &[AbiType::INT, AbiType::NET_ERROR]);
    pub const RESULT_UNIT_NET_ERROR: AbiType =
        AbiType::Apply(AbiConstructor::Result, &[AbiType::UNIT, AbiType::NET_ERROR]);
    pub const RESULT_SOCKET_ADDRESS_NET_ERROR: AbiType = AbiType::Apply(
        AbiConstructor::Result,
        &[AbiType::SOCKET_ADDRESS, AbiType::NET_ERROR],
    );
    pub const RESULT_TLS_STREAM_TLS_ERROR: AbiType = AbiType::Apply(
        AbiConstructor::Result,
        &[AbiType::TLS_STREAM, AbiType::TLS_ERROR],
    );
    pub const RESULT_TCP_READ_TLS_ERROR: AbiType = AbiType::Apply(
        AbiConstructor::Result,
        &[AbiType::TCP_READ, AbiType::TLS_ERROR],
    );
    pub const RESULT_INT_TLS_ERROR: AbiType =
        AbiType::Apply(AbiConstructor::Result, &[AbiType::INT, AbiType::TLS_ERROR]);
    pub const RESULT_UNIT_TLS_ERROR: AbiType =
        AbiType::Apply(AbiConstructor::Result, &[AbiType::UNIT, AbiType::TLS_ERROR]);
    pub const RESULT_SOCKET_ADDRESS_TLS_ERROR: AbiType = AbiType::Apply(
        AbiConstructor::Result,
        &[AbiType::SOCKET_ADDRESS, AbiType::TLS_ERROR],
    );

    /// The canonical text of this complete type expression.
    pub fn text(self) -> String {
        match self {
            AbiType::Primitive(primitive) => primitive.text().to_string(),
            AbiType::Core(core) => core.text().to_string(),
            AbiType::Native(native) => native.text().to_string(),
            AbiType::Var(index) => format!("${index}"),
            AbiType::List(element) => format!("List[{}]", element.text()),
            AbiType::Map(key, value) => format!("Map[{}, {}]", key.text(), value.text()),
            AbiType::Tuple(elements) => {
                let parts: Vec<String> = elements.iter().map(|element| element.text()).collect();
                format!("({})", parts.join(", "))
            }
            AbiType::Apply(constructor, arguments) => {
                let parts: Vec<String> = arguments.iter().map(|argument| argument.text()).collect();
                format!("{}[{}]", constructor.text(), parts.join(", "))
            }
        }
    }

    /// True when every generic constructor has its required arity.
    pub fn valid(self) -> bool {
        match self {
            AbiType::Primitive(_) | AbiType::Core(_) | AbiType::Native(_) | AbiType::Var(_) => true,
            AbiType::List(element) => element.valid(),
            AbiType::Map(key, value) => key.valid() && value.valid(),
            AbiType::Tuple(elements) => elements.iter().all(|element| element.valid()),
            AbiType::Apply(constructor, arguments) => {
                arguments.len() == constructor.arity()
                    && arguments.iter().all(|argument| argument.valid())
            }
        }
    }
}

/// The intrinsic ABI version.
///
/// Version 4 adds immutable Bytes operations and nominal builders.
/// Version 5 adds scalar text, shared views, byte search, and finish moves.
/// Version 6 adds bounded scans of active byte buffers.
/// Version 7 adds generic native collection operations.
/// Version 8 completes the mutable collection leaf operations.
/// Version 9 adds the list reorder marker.
/// Version 10 gives collection epoch exhaustion its own fault.
pub const INTRINSIC_ABI_VERSION: u32 = 10;

/// A dense intrinsic slot.
pub type IntrinsicSlot = u32;

/// One pure intrinsic definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntrinsicDef {
    pub name: &'static str,
    pub params: &'static [AbiType],
    pub reply: AbiType,
    pub semantic_revision: u32,
}

/// The `int.abs` intrinsic slot.
pub const INTRINSIC_INT_ABS: IntrinsicSlot = 0;
pub const INTRINSIC_INT_NEG: IntrinsicSlot = 1;
pub const INTRINSIC_INT_ADD: IntrinsicSlot = 2;
pub const INTRINSIC_INT_SUB: IntrinsicSlot = 3;
pub const INTRINSIC_INT_MUL: IntrinsicSlot = 4;
pub const INTRINSIC_INT_DIV: IntrinsicSlot = 5;
pub const INTRINSIC_INT_REM: IntrinsicSlot = 6;
pub const INTRINSIC_INT_EQ: IntrinsicSlot = 7;
pub const INTRINSIC_INT_NE: IntrinsicSlot = 8;
pub const INTRINSIC_INT_LT: IntrinsicSlot = 9;
pub const INTRINSIC_INT_LE: IntrinsicSlot = 10;
pub const INTRINSIC_INT_GT: IntrinsicSlot = 11;
pub const INTRINSIC_INT_GE: IntrinsicSlot = 12;
pub const INTRINSIC_BOOL_NOT: IntrinsicSlot = 13;
pub const INTRINSIC_BOOL_EQ: IntrinsicSlot = 14;
pub const INTRINSIC_BOOL_NE: IntrinsicSlot = 15;
pub const INTRINSIC_STRING_BYTE_LEN: IntrinsicSlot = 16;
pub const INTRINSIC_STRING_CHAR_COUNT: IntrinsicSlot = 17;
pub const INTRINSIC_STRING_CONCAT: IntrinsicSlot = 18;
pub const INTRINSIC_STRING_STARTS_WITH: IntrinsicSlot = 19;
pub const INTRINSIC_STRING_ENDS_WITH: IntrinsicSlot = 20;
pub const INTRINSIC_STRING_CONTAINS: IntrinsicSlot = 21;
pub const INTRINSIC_STRING_FIND_INDEX: IntrinsicSlot = 22;
pub const INTRINSIC_STRING_EQ: IntrinsicSlot = 23;
pub const INTRINSIC_STRING_NE: IntrinsicSlot = 24;
pub const INTRINSIC_BYTES_LEN: IntrinsicSlot = 25;
pub const INTRINSIC_BYTES_AT: IntrinsicSlot = 26;
pub const INTRINSIC_BYTES_GET: IntrinsicSlot = 27;
pub const INTRINSIC_BYTES_SLICE: IntrinsicSlot = 28;
pub const INTRINSIC_BYTES_CONCAT: IntrinsicSlot = 29;
pub const INTRINSIC_BYTES_STARTS_WITH: IntrinsicSlot = 30;
pub const INTRINSIC_BYTES_FIND_INDEX: IntrinsicSlot = 31;
pub const INTRINSIC_BYTES_HEX: IntrinsicSlot = 32;
pub const INTRINSIC_BYTES_IS_UTF8: IntrinsicSlot = 33;
pub const INTRINSIC_BYTES_TEXT: IntrinsicSlot = 34;
pub const INTRINSIC_BYTES_EQ: IntrinsicSlot = 35;
pub const INTRINSIC_BYTES_NE: IntrinsicSlot = 36;
pub const INTRINSIC_STRING_BUILDER_APPEND: IntrinsicSlot = 37;
pub const INTRINSIC_STRING_BUILDER_LEN: IntrinsicSlot = 38;
pub const INTRINSIC_STRING_BUILDER_CLEAR: IntrinsicSlot = 39;
pub const INTRINSIC_STRING_BUILDER_BUILD: IntrinsicSlot = 40;
pub const INTRINSIC_BYTE_BUFFER_APPEND: IntrinsicSlot = 41;
pub const INTRINSIC_BYTE_BUFFER_EXTEND: IntrinsicSlot = 42;
pub const INTRINSIC_BYTE_BUFFER_RESERVE: IntrinsicSlot = 43;
pub const INTRINSIC_BYTE_BUFFER_CLEAR: IntrinsicSlot = 44;
pub const INTRINSIC_BYTE_BUFFER_LEN: IntrinsicSlot = 45;
pub const INTRINSIC_BYTE_BUFFER_BUILD: IntrinsicSlot = 46;
pub const INTRINSIC_TEXT_AT: IntrinsicSlot = 47;
pub const INTRINSIC_TEXT_SLICE: IntrinsicSlot = 48;
pub const INTRINSIC_TEXT_IS_BOUNDARY: IntrinsicSlot = 49;
pub const INTRINSIC_TEXT_SLICE_BYTES: IntrinsicSlot = 50;
pub const INTRINSIC_TEXT_BYTES: IntrinsicSlot = 51;
pub const INTRINSIC_TEXT_LT: IntrinsicSlot = 52;
pub const INTRINSIC_TEXT_LE: IntrinsicSlot = 53;
pub const INTRINSIC_TEXT_GT: IntrinsicSlot = 54;
pub const INTRINSIC_TEXT_GE: IntrinsicSlot = 55;
pub const INTRINSIC_SUBSTRING_TO_STRING: IntrinsicSlot = 56;
pub const INTRINSIC_CHAR_CODEPOINT: IntrinsicSlot = 57;
pub const INTRINSIC_CHAR_UTF8_LEN: IntrinsicSlot = 58;
pub const INTRINSIC_CHAR_EQ: IntrinsicSlot = 59;
pub const INTRINSIC_CHAR_NE: IntrinsicSlot = 60;
pub const INTRINSIC_CHAR_LT: IntrinsicSlot = 61;
pub const INTRINSIC_CHAR_LE: IntrinsicSlot = 62;
pub const INTRINSIC_CHAR_GT: IntrinsicSlot = 63;
pub const INTRINSIC_CHAR_GE: IntrinsicSlot = 64;
pub const INTRINSIC_BYTES_COMPACT: IntrinsicSlot = 65;
pub const INTRINSIC_BYTES_TEXT_VIEW: IntrinsicSlot = 66;
pub const INTRINSIC_BYTES_LT: IntrinsicSlot = 67;
pub const INTRINSIC_BYTES_LE: IntrinsicSlot = 68;
pub const INTRINSIC_BYTES_GT: IntrinsicSlot = 69;
pub const INTRINSIC_BYTES_GE: IntrinsicSlot = 70;
pub const INTRINSIC_STRING_BUILDER_PUSH_CHAR: IntrinsicSlot = 71;
pub const INTRINSIC_STRING_BUILDER_BYTE_LEN: IntrinsicSlot = 72;
pub const INTRINSIC_STRING_BUILDER_FINISH: IntrinsicSlot = 73;
pub const INTRINSIC_BYTE_BUFFER_FINISH: IntrinsicSlot = 74;
pub const INTRINSIC_TEXT_FIND_BYTE_INDEX: IntrinsicSlot = 75;
pub const INTRINSIC_TEXT_AT_BYTE: IntrinsicSlot = 76;
pub const INTRINSIC_TEXT_TRIM: IntrinsicSlot = 77;
pub const INTRINSIC_TEXT_TRIM_START: IntrinsicSlot = 78;
pub const INTRINSIC_TEXT_TRIM_END: IntrinsicSlot = 79;
pub const INTRINSIC_TEXT_TO_LOWER_ASCII: IntrinsicSlot = 80;
pub const INTRINSIC_TEXT_TO_UPPER_ASCII: IntrinsicSlot = 81;
pub const INTRINSIC_TEXT_REPLACE: IntrinsicSlot = 82;
pub const INTRINSIC_TEXT_PARSE_INT_STATUS: IntrinsicSlot = 83;
pub const INTRINSIC_TEXT_PARSE_INT_VALUE: IntrinsicSlot = 84;
pub const INTRINSIC_BYTES_ENDS_WITH: IntrinsicSlot = 85;
pub const INTRINSIC_BYTES_CONTAINS: IntrinsicSlot = 86;
pub const INTRINSIC_TEXT_SPLIT: IntrinsicSlot = 87;
pub const INTRINSIC_TEXT_LINES: IntrinsicSlot = 88;
pub const INTRINSIC_BYTE_BUFFER_AT: IntrinsicSlot = 89;
pub const INTRINSIC_BYTE_BUFFER_FIND_FROM: IntrinsicSlot = 90;
pub const INTRINSIC_LIST_LEN: IntrinsicSlot = 91;
pub const INTRINSIC_LIST_AT: IntrinsicSlot = 92;
pub const INTRINSIC_LIST_GET: IntrinsicSlot = 93;
pub const INTRINSIC_LIST_PUSH: IntrinsicSlot = 94;
pub const INTRINSIC_MAP_LEN: IntrinsicSlot = 95;
pub const INTRINSIC_MAP_HAS: IntrinsicSlot = 96;
pub const INTRINSIC_MAP_AT: IntrinsicSlot = 97;
pub const INTRINSIC_MAP_GET: IntrinsicSlot = 98;
pub const INTRINSIC_MAP_PUT: IntrinsicSlot = 99;
pub const INTRINSIC_LIST_EPOCH: IntrinsicSlot = 100;
pub const INTRINSIC_LIST_ITER_LEN: IntrinsicSlot = 101;
pub const INTRINSIC_MAP_EPOCH: IntrinsicSlot = 102;
pub const INTRINSIC_MAP_ITER_LEN: IntrinsicSlot = 103;
pub const INTRINSIC_MAP_KEY_AT: IntrinsicSlot = 104;
pub const INTRINSIC_MAP_VALUE_AT: IntrinsicSlot = 105;
pub const INTRINSIC_LIST_CAPACITY: IntrinsicSlot = 106;
pub const INTRINSIC_LIST_SET: IntrinsicSlot = 107;
pub const INTRINSIC_LIST_POP: IntrinsicSlot = 108;
pub const INTRINSIC_LIST_INSERT: IntrinsicSlot = 109;
pub const INTRINSIC_LIST_REMOVE: IntrinsicSlot = 110;
pub const INTRINSIC_LIST_SWAP_REMOVE: IntrinsicSlot = 111;
pub const INTRINSIC_LIST_RESERVE: IntrinsicSlot = 112;
pub const INTRINSIC_LIST_TRUNCATE: IntrinsicSlot = 113;
pub const INTRINSIC_LIST_CONTAINS: IntrinsicSlot = 114;
pub const INTRINSIC_MAP_REMOVE: IntrinsicSlot = 115;
pub const INTRINSIC_MAP_CLEAR: IntrinsicSlot = 116;
pub const INTRINSIC_MAP_RESERVE: IntrinsicSlot = 117;
pub const INTRINSIC_LIST_REORDER: IntrinsicSlot = 118;

/// Pure intrinsics in stable slot order.
pub const INTRINSICS: [IntrinsicDef; 119] = [
    IntrinsicDef {
        name: "int.abs",
        params: &[AbiType::INT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "int.neg",
        params: &[AbiType::INT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "int.add",
        params: &[AbiType::INT, AbiType::INT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "int.sub",
        params: &[AbiType::INT, AbiType::INT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "int.mul",
        params: &[AbiType::INT, AbiType::INT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "int.div",
        params: &[AbiType::INT, AbiType::INT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "int.rem",
        params: &[AbiType::INT, AbiType::INT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "int.eq",
        params: &[AbiType::INT, AbiType::INT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "int.ne",
        params: &[AbiType::INT, AbiType::INT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "int.lt",
        params: &[AbiType::INT, AbiType::INT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "int.le",
        params: &[AbiType::INT, AbiType::INT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "int.gt",
        params: &[AbiType::INT, AbiType::INT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "int.ge",
        params: &[AbiType::INT, AbiType::INT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bool.not",
        params: &[AbiType::BOOL],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bool.eq",
        params: &[AbiType::BOOL, AbiType::BOOL],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bool.ne",
        params: &[AbiType::BOOL, AbiType::BOOL],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.byte_len",
        params: &[AbiType::TEXT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.len",
        params: &[AbiType::TEXT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.concat",
        params: &[AbiType::TEXT, AbiType::TEXT],
        reply: AbiType::STR,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.starts_with",
        params: &[AbiType::TEXT, AbiType::TEXT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.ends_with",
        params: &[AbiType::TEXT, AbiType::TEXT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.contains",
        params: &[AbiType::TEXT, AbiType::TEXT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.find_index",
        params: &[AbiType::TEXT, AbiType::TEXT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.eq",
        params: &[AbiType::TEXT, AbiType::TEXT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.ne",
        params: &[AbiType::TEXT, AbiType::TEXT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.len",
        params: &[AbiType::BYTES],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.at",
        params: &[AbiType::BYTES, AbiType::INT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.get",
        params: &[AbiType::BYTES, AbiType::INT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.slice",
        params: &[AbiType::BYTES, AbiType::INT, AbiType::INT],
        reply: AbiType::BYTES,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.concat",
        params: &[AbiType::BYTES, AbiType::BYTES],
        reply: AbiType::BYTES,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.starts_with",
        params: &[AbiType::BYTES, AbiType::BYTES],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.find_index",
        params: &[AbiType::BYTES, AbiType::BYTES],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.hex",
        params: &[AbiType::BYTES],
        reply: AbiType::STR,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.is_utf8",
        params: &[AbiType::BYTES],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.text",
        params: &[AbiType::BYTES],
        reply: AbiType::STR,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.eq",
        params: &[AbiType::BYTES, AbiType::BYTES],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.ne",
        params: &[AbiType::BYTES, AbiType::BYTES],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "string_builder.append",
        params: &[AbiType::STRING_BUILDER, AbiType::TEXT],
        reply: AbiType::STRING_BUILDER,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "string_builder.len",
        params: &[AbiType::STRING_BUILDER],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "string_builder.clear",
        params: &[AbiType::STRING_BUILDER],
        reply: AbiType::STRING_BUILDER,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "string_builder.build",
        params: &[AbiType::STRING_BUILDER],
        reply: AbiType::STR,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "byte_buffer.append",
        params: &[AbiType::BYTE_BUFFER, AbiType::INT],
        reply: AbiType::BYTE_BUFFER,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "byte_buffer.extend",
        params: &[AbiType::BYTE_BUFFER, AbiType::BYTES],
        reply: AbiType::BYTE_BUFFER,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "byte_buffer.reserve",
        params: &[AbiType::BYTE_BUFFER, AbiType::INT],
        reply: AbiType::BYTE_BUFFER,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "byte_buffer.clear",
        params: &[AbiType::BYTE_BUFFER],
        reply: AbiType::BYTE_BUFFER,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "byte_buffer.len",
        params: &[AbiType::BYTE_BUFFER],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "byte_buffer.build",
        params: &[AbiType::BYTE_BUFFER],
        reply: AbiType::BYTES,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.at",
        params: &[AbiType::TEXT, AbiType::INT],
        reply: AbiType::CHAR,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.slice",
        params: &[AbiType::TEXT, AbiType::INT, AbiType::INT],
        reply: AbiType::SUBSTRING,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.is_boundary",
        params: &[AbiType::TEXT, AbiType::INT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.slice_bytes",
        params: &[AbiType::TEXT, AbiType::INT, AbiType::INT],
        reply: AbiType::SUBSTRING,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.bytes",
        params: &[AbiType::TEXT],
        reply: AbiType::BYTES,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.lt",
        params: &[AbiType::TEXT, AbiType::TEXT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.le",
        params: &[AbiType::TEXT, AbiType::TEXT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.gt",
        params: &[AbiType::TEXT, AbiType::TEXT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.ge",
        params: &[AbiType::TEXT, AbiType::TEXT],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "substring.to_string",
        params: &[AbiType::SUBSTRING],
        reply: AbiType::STR,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "char.codepoint",
        params: &[AbiType::CHAR],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "char.utf8_len",
        params: &[AbiType::CHAR],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "char.eq",
        params: &[AbiType::CHAR, AbiType::CHAR],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "char.ne",
        params: &[AbiType::CHAR, AbiType::CHAR],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "char.lt",
        params: &[AbiType::CHAR, AbiType::CHAR],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "char.le",
        params: &[AbiType::CHAR, AbiType::CHAR],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "char.gt",
        params: &[AbiType::CHAR, AbiType::CHAR],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "char.ge",
        params: &[AbiType::CHAR, AbiType::CHAR],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.compact",
        params: &[AbiType::BYTES],
        reply: AbiType::BYTES,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.text_view",
        params: &[AbiType::BYTES],
        reply: AbiType::SUBSTRING,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.lt",
        params: &[AbiType::BYTES, AbiType::BYTES],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.le",
        params: &[AbiType::BYTES, AbiType::BYTES],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.gt",
        params: &[AbiType::BYTES, AbiType::BYTES],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.ge",
        params: &[AbiType::BYTES, AbiType::BYTES],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "string_builder.push_char",
        params: &[AbiType::STRING_BUILDER, AbiType::CHAR],
        reply: AbiType::STRING_BUILDER,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "string_builder.byte_len",
        params: &[AbiType::STRING_BUILDER],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "string_builder.finish",
        params: &[AbiType::STRING_BUILDER],
        reply: AbiType::STR,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "byte_buffer.finish",
        params: &[AbiType::BYTE_BUFFER],
        reply: AbiType::BYTES,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.find_byte_index",
        params: &[AbiType::TEXT, AbiType::TEXT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.at_byte",
        params: &[AbiType::TEXT, AbiType::INT],
        reply: AbiType::CHAR,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.trim",
        params: &[AbiType::TEXT],
        reply: AbiType::SUBSTRING,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.trim_start",
        params: &[AbiType::TEXT],
        reply: AbiType::SUBSTRING,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.trim_end",
        params: &[AbiType::TEXT],
        reply: AbiType::SUBSTRING,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.to_lower_ascii",
        params: &[AbiType::TEXT],
        reply: AbiType::STR,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.to_upper_ascii",
        params: &[AbiType::TEXT],
        reply: AbiType::STR,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.replace",
        params: &[AbiType::TEXT, AbiType::TEXT, AbiType::TEXT],
        reply: AbiType::STR,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.parse_int_status",
        params: &[AbiType::TEXT, AbiType::INT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.parse_int_value",
        params: &[AbiType::TEXT, AbiType::INT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.ends_with",
        params: &[AbiType::BYTES, AbiType::BYTES],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "bytes.contains",
        params: &[AbiType::BYTES, AbiType::BYTES],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.split",
        params: &[AbiType::TEXT, AbiType::TEXT],
        reply: AbiType::LIST_SUBSTRING,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "text.lines",
        params: &[AbiType::TEXT],
        reply: AbiType::LIST_SUBSTRING,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "byte_buffer.at",
        params: &[AbiType::BYTE_BUFFER, AbiType::INT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "byte_buffer.find_from",
        params: &[AbiType::BYTE_BUFFER, AbiType::BYTES, AbiType::INT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.len",
        params: &[AbiType::List(&AbiType::Var(0))],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.at",
        params: &[AbiType::List(&AbiType::Var(0)), AbiType::INT],
        reply: AbiType::Var(0),
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.get",
        params: &[AbiType::List(&AbiType::Var(0)), AbiType::INT],
        reply: AbiType::Apply(AbiConstructor::Option, &[AbiType::Var(0)]),
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.push",
        params: &[AbiType::List(&AbiType::Var(0)), AbiType::Var(0)],
        reply: AbiType::UNIT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "map.len",
        params: &[AbiType::Map(&AbiType::Var(0), &AbiType::Var(1))],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "map.has",
        params: &[
            AbiType::Map(&AbiType::Var(0), &AbiType::Var(1)),
            AbiType::Var(0),
        ],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "map.at",
        params: &[
            AbiType::Map(&AbiType::Var(0), &AbiType::Var(1)),
            AbiType::Var(0),
        ],
        reply: AbiType::Var(1),
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "map.get",
        params: &[
            AbiType::Map(&AbiType::Var(0), &AbiType::Var(1)),
            AbiType::Var(0),
        ],
        reply: AbiType::Apply(AbiConstructor::Option, &[AbiType::Var(1)]),
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "map.put",
        params: &[
            AbiType::Map(&AbiType::Var(0), &AbiType::Var(1)),
            AbiType::Var(0),
            AbiType::Var(1),
        ],
        reply: AbiType::Apply(AbiConstructor::Option, &[AbiType::Var(1)]),
        semantic_revision: 2,
    },
    IntrinsicDef {
        name: "list.epoch",
        params: &[AbiType::List(&AbiType::Var(0))],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.iter_len",
        params: &[AbiType::List(&AbiType::Var(0)), AbiType::INT],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "map.epoch",
        params: &[AbiType::Map(&AbiType::Var(0), &AbiType::Var(1))],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "map.iter_len",
        params: &[
            AbiType::Map(&AbiType::Var(0), &AbiType::Var(1)),
            AbiType::INT,
        ],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "map.key_at",
        params: &[
            AbiType::Map(&AbiType::Var(0), &AbiType::Var(1)),
            AbiType::INT,
        ],
        reply: AbiType::Var(0),
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "map.value_at",
        params: &[
            AbiType::Map(&AbiType::Var(0), &AbiType::Var(1)),
            AbiType::INT,
        ],
        reply: AbiType::Var(1),
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.capacity",
        params: &[AbiType::List(&AbiType::Var(0))],
        reply: AbiType::INT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.set",
        params: &[
            AbiType::List(&AbiType::Var(0)),
            AbiType::INT,
            AbiType::Var(0),
        ],
        reply: AbiType::UNIT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.pop",
        params: &[AbiType::List(&AbiType::Var(0))],
        reply: AbiType::Apply(AbiConstructor::Option, &[AbiType::Var(0)]),
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.insert",
        params: &[
            AbiType::List(&AbiType::Var(0)),
            AbiType::INT,
            AbiType::Var(0),
        ],
        reply: AbiType::UNIT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.remove",
        params: &[AbiType::List(&AbiType::Var(0)), AbiType::INT],
        reply: AbiType::Var(0),
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.swap_remove",
        params: &[AbiType::List(&AbiType::Var(0)), AbiType::INT],
        reply: AbiType::Var(0),
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.reserve",
        params: &[AbiType::List(&AbiType::Var(0)), AbiType::INT],
        reply: AbiType::UNIT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.truncate",
        params: &[AbiType::List(&AbiType::Var(0)), AbiType::INT],
        reply: AbiType::UNIT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.contains",
        params: &[AbiType::List(&AbiType::Var(0)), AbiType::Var(0)],
        reply: AbiType::BOOL,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "map.remove",
        params: &[
            AbiType::Map(&AbiType::Var(0), &AbiType::Var(1)),
            AbiType::Var(0),
        ],
        reply: AbiType::Apply(AbiConstructor::Option, &[AbiType::Var(1)]),
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "map.clear",
        params: &[AbiType::Map(&AbiType::Var(0), &AbiType::Var(1))],
        reply: AbiType::UNIT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "map.reserve",
        params: &[
            AbiType::Map(&AbiType::Var(0), &AbiType::Var(1)),
            AbiType::INT,
        ],
        reply: AbiType::UNIT,
        semantic_revision: 1,
    },
    IntrinsicDef {
        name: "list.reorder",
        params: &[AbiType::List(&AbiType::Var(0))],
        reply: AbiType::UNIT,
        semantic_revision: 1,
    },
];

/// The number of pure intrinsic slots.
pub const INTRINSIC_COUNT: u32 = INTRINSICS.len() as u32;

/// Return one intrinsic definition.
pub fn intrinsic(slot: IntrinsicSlot) -> &'static IntrinsicDef {
    &INTRINSICS[slot as usize]
}

/// Find one intrinsic by its stable name.
pub fn intrinsic_by_name(name: &str) -> Option<IntrinsicSlot> {
    INTRINSICS
        .iter()
        .position(|def| def.name == name)
        .map(|index| index as u32)
}

/// Return the stable identity of one intrinsic.
pub fn intrinsic_identity(slot: IntrinsicSlot) -> [u8; 32] {
    let def = intrinsic(slot);
    let mut input = Vec::new();
    input.extend_from_slice(b"lm-intrinsic-v1\0");
    input.extend_from_slice(&INTRINSIC_ABI_VERSION.to_le_bytes());
    id_field(&mut input, def.name.as_bytes());
    id_field(&mut input, &(def.params.len() as u64).to_le_bytes());
    for param in def.params {
        id_field(&mut input, param.text().as_bytes());
    }
    id_field(&mut input, def.reply.text().as_bytes());
    id_field(&mut input, &def.semantic_revision.to_le_bytes());
    sha256(&input)
}

/// Return the digest of the intrinsic manifest.
pub fn intrinsic_manifest_digest() -> [u8; 32] {
    let mut input = Vec::new();
    input.extend_from_slice(b"lm-intrinsics-manifest-v1\0");
    input.extend_from_slice(&INTRINSIC_ABI_VERSION.to_le_bytes());
    for slot in 0..INTRINSIC_COUNT {
        input.extend_from_slice(&intrinsic_identity(slot));
    }
    sha256(&input)
}

/// The behavior family of one operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    /// A host operation with a fixed first-order signature. It is
    /// callable through `sys`, first-class as an `Op` value, and a
    /// valid `mock` and `Call` pattern target.
    Fixed,
    /// A VM control operation. Its signature is generic and the
    /// verifier applies a built-in rule per slot. It is not
    /// first-class and cannot be mocked.
    VmControl,
}

impl OpKind {
    /// The canonical text of the kind, for identity hashing.
    pub fn tag(self) -> &'static str {
        match self {
            OpKind::Fixed => "fixed",
            OpKind::VmControl => "vm-control",
        }
    }
}

/// One manifest entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpDef {
    /// The group name, for example `Io`.
    pub group: &'static str,
    /// The member name, for example `Print`.
    pub member: &'static str,
    pub kind: OpKind,
    /// Parameter types. Empty for `VmControl` entries: their generic
    /// schemas live in the verifier rules.
    pub params: &'static [AbiType],
    /// The reply type. `Unit` for `VmControl` entries.
    pub reply: AbiType,
    /// The generic schema text of a `VmControl` entry, for identity
    /// hashing. Empty for `Fixed` entries.
    pub schema: &'static str,
    /// The snapshot classification of one pending instance of this
    /// operation (specification 16.4 and 25.5).
    ///
    /// `HostAttachment` means the operation may suspend and hold live
    /// state outside every machine while it waits. `MachineState`
    /// means the operation always completes inside the host call, so
    /// it leaves nothing pending for a snapshot to copy. A host that
    /// suspends a `MachineState` operation breaks the contract, and
    /// the VM rejects that reply.
    pub snapshot: SnapshotClass,
}

impl OpDef {
    /// True when a pending instance of this operation is a live host
    /// attachment.
    pub fn suspends(&self) -> bool {
        matches!(self.snapshot, SnapshotClass::HostAttachment)
    }
}

/// Dense slots for the week-4 operations. The constants match the
/// index in `OPS`.
pub const OP_IO_PRINT: OpSlot = 0;
pub const OP_IO_ERROR: OpSlot = 1;
pub const OP_IO_READ_LINE: OpSlot = 2;
pub const OP_CLOCK_NOW: OpSlot = 3;
pub const OP_CLOCK_MONOTONIC: OpSlot = 4;
pub const OP_CLOCK_SLEEP: OpSlot = 5;
pub const OP_RAND_INT: OpSlot = 6;
pub const OP_VM_NEW: OpSlot = 7;
pub const OP_VM_FROM_FN: OpSlot = 8;
pub const OP_VM_RUN: OpSlot = 9;
pub const OP_VM_STEP: OpSlot = 10;
pub const OP_VM_DRIVE: OpSlot = 11;
pub const OP_VM_ANSWER: OpSlot = 12;
pub const OP_VM_REJECT: OpSlot = 13;
pub const OP_VM_DISPATCH: OpSlot = 14;
pub const OP_VM_TABLE: OpSlot = 15;
pub const OP_PROC_RUN: OpSlot = 16;
pub const OP_PROC_SPAWN: OpSlot = 17;
pub const OP_PROC_SEND: OpSlot = 18;
pub const OP_PROC_CLOSE: OpSlot = 19;
pub const OP_PROC_RECV: OpSlot = 20;
pub const OP_PROC_DONE: OpSlot = 21;
pub const OP_PROC_PAUSE: OpSlot = 22;
pub const OP_PROC_RESUME: OpSlot = 23;
pub const OP_VM_SNAPSHOT_HELD: OpSlot = 24;
pub const OP_VM_SNAPSHOT_SELF: OpSlot = 25;
pub const OP_VM_LOAD_SNAPSHOT: OpSlot = 26;
pub const OP_VM_RESTORE: OpSlot = 27;
pub const OP_FS_OPEN: OpSlot = 28;
pub const OP_FS_READ: OpSlot = 29;
pub const OP_FS_WRITE: OpSlot = 30;
pub const OP_FS_SEEK: OpSlot = 31;
pub const OP_FS_FLUSH: OpSlot = 32;
pub const OP_FS_CLOSE: OpSlot = 33;
pub const OP_VM_HANDLES: OpSlot = 34;
pub const OP_VM_RESOURCE: OpSlot = 35;
pub const OP_VM_SERVE_FILE: OpSlot = 36;
pub const OP_VM_RESOURCE_IS_OPEN: OpSlot = 37;
pub const OP_VM_RESOURCE_CLOSE: OpSlot = 38;
pub const OP_VM_RESOURCE_KIND: OpSlot = 39;
pub const OP_PROC_SNAPSHOT_WAIT: OpSlot = 40;
pub const OP_VM_RESOURCE_SAME: OpSlot = 41;
pub const OP_VM_DRIVE_WAIT: OpSlot = 42;
pub const OP_PROC_RECV_WAIT: OpSlot = 43;
pub const OP_WAIT_WAIT: OpSlot = 44;
pub const OP_WAIT_CHOOSE: OpSlot = 45;
pub const OP_WAIT_CANCEL: OpSlot = 46;
pub const OP_VM_DRIVE_FOR: OpSlot = 47;
pub const OP_VM_SNAPSHOT_WAIT_HELD: OpSlot = 48;
pub const OP_DNS_RESOLVE: OpSlot = 49;
pub const OP_TCP_CONNECT: OpSlot = 50;
pub const OP_TCP_LISTEN: OpSlot = 51;
pub const OP_TCP_ACCEPT: OpSlot = 52;
pub const OP_TCP_READ: OpSlot = 53;
pub const OP_TCP_WRITE: OpSlot = 54;
pub const OP_TCP_SHUTDOWN: OpSlot = 55;
pub const OP_TCP_LOCAL_ADDRESS: OpSlot = 56;
pub const OP_TCP_PEER_ADDRESS: OpSlot = 57;
pub const OP_TCP_CLOSE: OpSlot = 58;
pub const OP_VM_SERVE_TCP_STREAM: OpSlot = 59;
pub const OP_VM_SERVE_TCP_LISTENER: OpSlot = 60;
pub const OP_TLS_HANDSHAKE: OpSlot = 61;
pub const OP_TLS_READ: OpSlot = 62;
pub const OP_TLS_WRITE: OpSlot = 63;
pub const OP_TLS_SHUTDOWN: OpSlot = 64;
pub const OP_TLS_LOCAL_ADDRESS: OpSlot = 65;
pub const OP_TLS_PEER_ADDRESS: OpSlot = 66;
pub const OP_TLS_CLOSE: OpSlot = 67;
pub const OP_VM_SERVE_TLS_STREAM: OpSlot = 68;

/// The exact operations, in canonical slot order.
pub const OPS: [OpDef; 70] = [
    OpDef {
        group: "Io",
        member: "Print",
        kind: OpKind::Fixed,
        params: &[AbiType::STR],
        reply: AbiType::UNIT,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Io",
        member: "Error",
        kind: OpKind::Fixed,
        params: &[AbiType::STR],
        reply: AbiType::UNIT,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Io",
        member: "ReadLine",
        kind: OpKind::Fixed,
        params: &[],
        reply: AbiType::RESULT_OPTION_STR_IO_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Clock",
        member: "Now",
        kind: OpKind::Fixed,
        params: &[],
        reply: AbiType::INT,
        schema: "",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Clock",
        member: "Monotonic",
        kind: OpKind::Fixed,
        params: &[],
        reply: AbiType::INT,
        schema: "",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Clock",
        member: "Sleep",
        kind: OpKind::Fixed,
        params: &[AbiType::INT],
        reply: AbiType::UNIT,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Rand",
        member: "Int",
        kind: OpKind::Fixed,
        params: &[AbiType::INT, AbiType::INT],
        reply: AbiType::INT,
        schema: "",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "New",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "() -> EmptyVm",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "FromFn",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[A,T,e](EmptyVm, Fn[A,T,e], control A) -> Vm[T]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Run",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T]) -> RunResult[T]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Step",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T]) -> StepEvent[T]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Drive",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T]) -> DriveEvent[T]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Answer",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T,A,R](Vm[T], PendingCall[A,R], R) -> ()",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Reject",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T], Request, Fault) -> ()",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Dispatch",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T], Request) -> ()",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Table",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T]) -> PolicyTable",
        snapshot: SnapshotClass::MachineState,
    },
    // The proc operations of specification 23.6. Every one of them is
    // machine state: a blocked proc call waits on another machine of
    // the same machine world, never on live state outside it. The
    // scheduler record that carries the block holds proc identifiers
    // and ordinals only, so a snapshot rebuilds it from the machines.
    OpDef {
        group: "Proc",
        member: "Run",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[M,R](Vm[R], Type[M]) -> Handle[M,R]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "Spawn",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[M,R,A](Class[Proc[M]], control A) -> Handle[M,R]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "Send",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[M,R](Handle[M,R], M) -> SendResult",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "Close",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[M,R](Handle[M,R]) -> SendResult",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "Recv",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[M](proc self) -> Recv[M]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "Done",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[M,R](Handle[M,R]) -> ProcResult[R]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "Pause",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[M,R](Handle[M,R]) -> Result[Vm[R], ProcError]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "Resume",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[M,R](Handle[M,R]) -> Result[(), ProcError]",
        snapshot: SnapshotClass::MachineState,
    },
    // The snapshot operations of specification 23.5. A capture, a
    // load, and a restore all run inside the driver loop, so none of
    // them holds live state outside a machine.
    OpDef {
        group: "Vm",
        member: "SnapshotHeld",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T]) -> Result[Snapshot[T], SnapshotError]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "SnapshotSelf",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::RESULT_SNAPSHOT_IMAGE_ERROR,
        schema: "() -> Result[SnapshotImage, SnapshotError]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "LoadSnapshot",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "(Bytes) -> Result[SnapshotImage, SnapshotError]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Restore",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](EmptyVm, Snapshot[T]) -> Result[Vm[T], RestoreError]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Fs",
        member: "Open",
        kind: OpKind::Fixed,
        params: &[AbiType::STR, AbiType::OPEN_OPTIONS],
        reply: AbiType::RESULT_FILE_HANDLE_FS_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Fs",
        member: "Read",
        kind: OpKind::Fixed,
        params: &[AbiType::FILE_HANDLE, AbiType::INT],
        reply: AbiType::RESULT_BYTES_FS_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Fs",
        member: "Write",
        kind: OpKind::Fixed,
        params: &[AbiType::FILE_HANDLE, AbiType::BYTES],
        reply: AbiType::RESULT_INT_FS_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Fs",
        member: "Seek",
        kind: OpKind::Fixed,
        params: &[AbiType::FILE_HANDLE, AbiType::SEEK_FROM],
        reply: AbiType::RESULT_INT_FS_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Fs",
        member: "Flush",
        kind: OpKind::Fixed,
        params: &[AbiType::FILE_HANDLE],
        reply: AbiType::RESULT_UNIT_FS_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Fs",
        member: "Close",
        kind: OpKind::Fixed,
        params: &[AbiType::FILE_HANDLE],
        reply: AbiType::RESULT_UNIT_FS_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Vm",
        member: "Handles",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T]) -> List[ResourceHandle]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Resource",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T,R: FileHandle | TcpResource](Vm[T], R) -> ResourceHandle",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "ServeFile",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T], PendingCall[(String, OpenOptions), Result[FileHandle, FsError]]) -> ResourceHandle",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "ResourceIsOpen",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "(ResourceHandle) -> Bool",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "ResourceClose",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "(ResourceHandle) -> Bool",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "ResourceKind",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "(ResourceHandle) -> String",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "SnapshotWait",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[M,R](Handle[M,R], Int) -> Result[Snapshot[R], SnapshotError]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "ResourceSame",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "(ResourceHandle, ResourceHandle) -> Bool",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "DriveWait",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T]) -> Wait[DriveEvent[T]]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "RecvWait",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[M](proc self) -> Wait[Recv[M]]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Wait",
        member: "Wait",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Wait[T]) -> T",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Wait",
        member: "Choose",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[A,B](Wait[A], Wait[B]) -> Wait[Choice[A,B]]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Wait",
        member: "Cancel",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Wait[T]) -> Bool",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "DriveFor",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T], Int) -> Option[DriveEvent[T]]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "SnapshotWaitHeld",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T], Int) -> Result[Snapshot[T], SnapshotError]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Dns",
        member: "Resolve",
        kind: OpKind::Fixed,
        params: &[AbiType::STR, AbiType::INT],
        reply: AbiType::RESULT_LIST_SOCKET_ADDRESS_NET_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tcp",
        member: "Connect",
        kind: OpKind::Fixed,
        params: &[AbiType::SOCKET_ADDRESS],
        reply: AbiType::RESULT_TCP_STREAM_NET_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tcp",
        member: "Listen",
        kind: OpKind::Fixed,
        params: &[AbiType::SOCKET_ADDRESS, AbiType::INT],
        reply: AbiType::RESULT_TCP_LISTENER_NET_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tcp",
        member: "Accept",
        kind: OpKind::Fixed,
        params: &[AbiType::TCP_LISTENER],
        reply: AbiType::RESULT_ACCEPT_NET_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tcp",
        member: "Read",
        kind: OpKind::Fixed,
        params: &[AbiType::TCP_STREAM, AbiType::INT],
        reply: AbiType::RESULT_TCP_READ_NET_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tcp",
        member: "Write",
        kind: OpKind::Fixed,
        params: &[AbiType::TCP_STREAM, AbiType::BYTES],
        reply: AbiType::RESULT_INT_NET_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tcp",
        member: "Shutdown",
        kind: OpKind::Fixed,
        params: &[AbiType::TCP_STREAM, AbiType::SHUTDOWN],
        reply: AbiType::RESULT_UNIT_NET_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tcp",
        member: "LocalAddress",
        kind: OpKind::Fixed,
        params: &[AbiType::TCP_RESOURCE],
        reply: AbiType::RESULT_SOCKET_ADDRESS_NET_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tcp",
        member: "PeerAddress",
        kind: OpKind::Fixed,
        params: &[AbiType::TCP_STREAM],
        reply: AbiType::RESULT_SOCKET_ADDRESS_NET_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tcp",
        member: "Close",
        kind: OpKind::Fixed,
        params: &[AbiType::TCP_RESOURCE],
        reply: AbiType::RESULT_UNIT_NET_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Vm",
        member: "ServeTcpStream",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T], PendingCall, SocketAddress) -> ResourceHandle",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "ServeTcpListener",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T], PendingCall[SocketAddress, Result[TcpListener, NetError]]) -> ResourceHandle",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Tls",
        member: "Handshake",
        kind: OpKind::Fixed,
        params: &[
            AbiType::TCP_STREAM,
            AbiType::STR,
            AbiType::INT,
            AbiType::LIST_BYTES,
            AbiType::LIST_BYTES,
            AbiType::INT,
            AbiType::INT,
        ],
        reply: AbiType::RESULT_TLS_STREAM_TLS_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tls",
        member: "Read",
        kind: OpKind::Fixed,
        params: &[AbiType::TLS_STREAM, AbiType::INT],
        reply: AbiType::RESULT_TCP_READ_TLS_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tls",
        member: "Write",
        kind: OpKind::Fixed,
        params: &[AbiType::TLS_STREAM, AbiType::BYTES],
        reply: AbiType::RESULT_INT_TLS_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tls",
        member: "Shutdown",
        kind: OpKind::Fixed,
        params: &[AbiType::TLS_STREAM],
        reply: AbiType::RESULT_UNIT_TLS_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tls",
        member: "LocalAddress",
        kind: OpKind::Fixed,
        params: &[AbiType::TLS_STREAM],
        reply: AbiType::RESULT_SOCKET_ADDRESS_TLS_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tls",
        member: "PeerAddress",
        kind: OpKind::Fixed,
        params: &[AbiType::TLS_STREAM],
        reply: AbiType::RESULT_SOCKET_ADDRESS_TLS_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Tls",
        member: "Close",
        kind: OpKind::Fixed,
        params: &[AbiType::TLS_STREAM],
        reply: AbiType::RESULT_UNIT_TLS_ERROR,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Vm",
        member: "ServeTlsStream",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::UNIT,
        schema: "[T](Vm[T], PendingCall) -> ResourceHandle",
        snapshot: SnapshotClass::MachineState,
    },
    // The search choice point of a driver. The operation has no host
    // implementation, because no host can answer it: a choice point
    // means something only to a driver that explores the branches.
    // A table denies by default, so a program that performs it with no
    // driver faults with `PolicyDenied`.
    //
    // The argument is the number of candidates, and the reply is the
    // index of one candidate. The driver therefore reads one integer
    // and writes one integer, and it never reads a guest value. One
    // driver serves every searched program.
    //
    // A pending pick holds no host state, so it never blocks a
    // capture. This is the property the whole design rests on: a
    // driver captures the world at the choice point and restores one
    // world for each candidate.
    OpDef {
        group: "Choose",
        member: "Pick",
        kind: OpKind::Fixed,
        params: &[AbiType::INT],
        reply: AbiType::INT,
        schema: "",
        snapshot: SnapshotClass::MachineState,
    },
];

/// The number of exact operations.
pub const OP_COUNT: u32 = OPS.len() as u32;

/// The number of groups.
pub const GROUP_COUNT: u32 = GROUPS.len() as u32;

/// Return the definition of one operation slot.
pub fn op(slot: OpSlot) -> &'static OpDef {
    &OPS[slot as usize]
}

/// The canonical qualified name of one operation, for example
/// `Io.Print`.
pub fn op_name(slot: OpSlot) -> String {
    let def = op(slot);
    format!("{}.{}", def.group, def.member)
}

/// The group slot of one operation.
pub fn op_group(slot: OpSlot) -> GroupSlot {
    let def = op(slot);
    group_by_name(def.group).expect("every operation names a manifest group")
}

/// Find a group slot by name.
pub fn group_by_name(name: &str) -> Option<GroupSlot> {
    GROUPS.iter().position(|g| *g == name).map(|i| i as u32)
}

/// Return the direct manifest members of one effect-set slot.
pub fn group_members(slot: GroupSlot) -> &'static [&'static str] {
    GROUP_MEMBERS[slot as usize]
}

fn group_contains_op_uncached(group: GroupSlot, operation: OpSlot, seen: &mut [bool]) -> bool {
    fn contains(group: GroupSlot, operation: OpSlot, seen: &mut [bool]) -> bool {
        let at = group as usize;
        if seen[at] {
            return false;
        }
        seen[at] = true;
        let name = GROUPS[at];
        if op(operation).group == name {
            return true;
        }
        let exact = op_name(operation);
        for member in GROUP_MEMBERS[at] {
            if *member == exact {
                return true;
            }
            if let Some(child) = group_by_name(member) {
                if contains(child, operation, seen) {
                    return true;
                }
            }
        }
        false
    }

    contains(group, operation, seen)
}

fn group_operation_bits() -> &'static [Vec<bool>] {
    static BITS: std::sync::OnceLock<Vec<Vec<bool>>> = std::sync::OnceLock::new();
    BITS.get_or_init(|| {
        (0..GROUP_COUNT)
            .map(|group| {
                (0..OP_COUNT)
                    .map(|operation| {
                        let mut seen = vec![false; GROUP_COUNT as usize];
                        group_contains_op_uncached(group, operation, &mut seen)
                    })
                    .collect()
            })
            .collect()
    })
}

/// True when one group or effect set contains an exact operation.
pub fn group_contains_op(group: GroupSlot, operation: OpSlot) -> bool {
    group_operation_bits()[group as usize][operation as usize]
}

/// Return the exact operation closure of one group or effect set.
pub fn group_operations(group: GroupSlot) -> Vec<OpSlot> {
    group_operation_bits()[group as usize]
        .iter()
        .enumerate()
        .filter_map(|(operation, included)| included.then_some(operation as OpSlot))
        .collect()
}

/// Return the exact operation closure of one valid row name.
pub fn row_name_operations(name: &str) -> Option<Vec<OpSlot>> {
    if let Some(operation) = op_by_name(name) {
        return Some(vec![operation]);
    }
    group_by_name(name).map(group_operations)
}

/// True when one row name is included in another row name.
pub fn row_name_included(name: &str, expected: &str) -> bool {
    if name == expected {
        return true;
    }
    let Some(actual) = row_name_operations(name) else {
        return false;
    };
    let Some(required) = row_name_operations(expected) else {
        return false;
    };
    if actual.is_empty() || required.is_empty() {
        return false;
    }
    actual.iter().all(|operation| required.contains(operation))
}

/// Find an operation slot by its canonical qualified name.
pub fn op_by_name(name: &str) -> Option<OpSlot> {
    OPS.iter()
        .position(|def| {
            name.len() == def.group.len() + def.member.len() + 1
                && name.starts_with(def.group)
                && name.as_bytes().get(def.group.len()) == Some(&b'.')
                && name.ends_with(def.member)
        })
        .map(|i| i as u32)
}

/// Validate every name, member, type, and dependency in the manifest.
pub fn validate_manifest() -> Result<(), String> {
    if GROUPS.len() != GROUP_MEMBERS.len() {
        return Err("the group and member tables have different lengths".to_string());
    }
    let mut names = std::collections::BTreeSet::new();
    for group in GROUPS {
        if !names.insert(group) {
            return Err(format!("the manifest has duplicate group `{group}`"));
        }
    }
    let mut operations = std::collections::BTreeSet::new();
    for (slot, def) in OPS.iter().enumerate() {
        if group_by_name(def.group).is_none() {
            return Err(format!(
                "operation {slot} names unknown group `{}`",
                def.group
            ));
        }
        if !def.reply.valid() || def.params.iter().any(|param| !param.valid()) {
            return Err(format!("operation {slot} has an invalid type expression"));
        }
        let name = op_name(slot as OpSlot);
        if !operations.insert(name.clone()) {
            return Err(format!("the manifest has duplicate operation `{name}`"));
        }
        if group_by_name(&name).is_some() {
            return Err(format!("operation `{name}` collides with an effect set"));
        }
    }
    for (group, members) in GROUPS.iter().zip(GROUP_MEMBERS.iter()) {
        let mut direct = std::collections::BTreeSet::new();
        for member in *members {
            if !direct.insert(*member) {
                return Err(format!("effect set `{group}` repeats member `{member}`"));
            }
            if op_by_name(member).is_none() && group_by_name(member).is_none() {
                return Err(format!(
                    "effect set `{group}` names unknown member `{member}`"
                ));
            }
        }
    }

    fn visit(group: GroupSlot, state: &mut [u8]) -> Result<(), String> {
        let at = group as usize;
        if state[at] == 1 {
            return Err(format!("effect set `{}` is part of a cycle", GROUPS[at]));
        }
        if state[at] == 2 {
            return Ok(());
        }
        state[at] = 1;
        for member in GROUP_MEMBERS[at] {
            if let Some(child) = group_by_name(member) {
                visit(child, state)?;
            }
        }
        state[at] = 2;
        Ok(())
    }

    let mut state = vec![0; GROUP_COUNT as usize];
    for group in 0..GROUP_COUNT {
        visit(group, &mut state)?;
    }
    Ok(())
}

/// Find a `Fixed` operation inside a group by its member name. This
/// is the lookup behind `sys.<group>.<Member>`.
pub fn fixed_member(group: &str, member: &str) -> Option<OpSlot> {
    OPS.iter()
        .position(|def| def.group == group && def.member == member && def.kind == OpKind::Fixed)
        .map(|i| i as u32)
}

/// True when a row name is valid: an exact operation name or a group
/// name.
pub fn row_name_valid(name: &str) -> bool {
    op_by_name(name).is_some() || group_by_name(name).is_some()
}

/// The stable identity hash of one operation: the domain-separated
/// SHA-256 of the ABI version, the qualified name, the complete
/// signature or schema text, and every other semantic field.
pub fn op_identity(slot: OpSlot) -> [u8; 32] {
    identity_of(&op_name(slot), op(slot))
}

/// One length-prefixed field of an identity encoding.
///
/// The length prefix keeps two field lists apart, so no pair of
/// definitions shares one byte string.
fn id_field(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// The stable identity hash of one operation definition.
///
/// The hash covers every field of `OpDef`, through one common encoder.
/// The encoder lists the fields of the structure, not the fields one
/// variant happens to use, so a later variant cannot omit a field.
///
/// An earlier encoder read `params` and `reply` for a `Fixed` entry and
/// `schema` for a `VmControl` entry. `Vm.SnapshotSelf` is `VmControl`
/// with a reply the verifier reads, so that reply could change and move
/// no digest.
///
/// The snapshot classification is one field of the same list. It
/// decides whether a pending instance holds live host state, so it
/// changes snapshot and resource behavior. A change to any field moves
/// the operation identity, the manifest digest, and the verification
/// hash of every module that names the operation.
///
/// The call takes the definition, so a test can hash a changed
/// definition without a second manifest.
pub fn identity_of(name: &str, def: &OpDef) -> [u8; 32] {
    let mut input = Vec::new();
    input.extend_from_slice(b"lm-operation-v3\0");
    input.extend_from_slice(&ABI_VERSION.to_le_bytes());
    // The qualified name, then every field of `OpDef` in declaration
    // order. Add the new field here when `OpDef` grows one.
    id_field(&mut input, name.as_bytes());
    id_field(&mut input, def.group.as_bytes());
    id_field(&mut input, def.member.as_bytes());
    id_field(&mut input, def.kind.tag().as_bytes());
    id_field(&mut input, &(def.params.len() as u64).to_le_bytes());
    for param in def.params {
        id_field(&mut input, param.text().as_bytes());
    }
    id_field(&mut input, def.reply.text().as_bytes());
    id_field(&mut input, def.schema.as_bytes());
    id_field(&mut input, def.snapshot.tag().as_bytes());
    sha256(&input)
}

/// The digest of the full manifest: version, groups, and every
/// operation identity in slot order.
pub fn manifest_digest() -> [u8; 32] {
    static CACHE: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    *CACHE.get_or_init(manifest_digest_uncached)
}

/// The digest, computed from the compiled tables.
///
/// The manifest is compile-time data, so the digest never changes in
/// one process. The caller above computes it once.
fn manifest_digest_uncached() -> [u8; 32] {
    validate_manifest().expect("the compiled operation manifest is valid");
    let identities: Vec<[u8; 32]> = (0..OP_COUNT).map(op_identity).collect();
    manifest_digest_of(&identities)
}

/// The digest of one operation identity list.
///
/// The call takes the identities, so a test can state what one changed
/// definition does to the manifest and to every digest above it.
pub fn manifest_digest_of(identities: &[[u8; 32]]) -> [u8; 32] {
    let mut input = Vec::new();
    input.extend_from_slice(b"lm-operations-manifest-v1\0");
    input.extend_from_slice(&ABI_VERSION.to_le_bytes());
    for (group, members) in GROUPS.iter().zip(GROUP_MEMBERS.iter()) {
        id_field(&mut input, group.as_bytes());
        id_field(&mut input, &(members.len() as u64).to_le_bytes());
        for member in *members {
            id_field(&mut input, member.as_bytes());
        }
    }
    for id in identities {
        input.extend_from_slice(id);
    }
    sha256(&input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_match_the_constants() {
        assert_eq!(op_by_name("Io.Print"), Some(OP_IO_PRINT));
        assert_eq!(op_by_name("Io.Error"), Some(OP_IO_ERROR));
        assert_eq!(op_by_name("Io.ReadLine"), Some(OP_IO_READ_LINE));
        assert_eq!(op_by_name("Clock.Now"), Some(OP_CLOCK_NOW));
        assert_eq!(op_by_name("Clock.Monotonic"), Some(OP_CLOCK_MONOTONIC));
        assert_eq!(op_by_name("Clock.Sleep"), Some(OP_CLOCK_SLEEP));
        assert_eq!(op_by_name("Rand.Int"), Some(OP_RAND_INT));
        assert_eq!(op_by_name("Vm.New"), Some(OP_VM_NEW));
        assert_eq!(op_by_name("Vm.Table"), Some(OP_VM_TABLE));
        assert_eq!(op_by_name("Proc.Run"), Some(OP_PROC_RUN));
        assert_eq!(op_by_name("Proc.Spawn"), Some(OP_PROC_SPAWN));
        assert_eq!(op_by_name("Proc.Send"), Some(OP_PROC_SEND));
        assert_eq!(op_by_name("Proc.Close"), Some(OP_PROC_CLOSE));
        assert_eq!(op_by_name("Proc.Recv"), Some(OP_PROC_RECV));
        assert_eq!(op_by_name("Proc.Done"), Some(OP_PROC_DONE));
        assert_eq!(op_by_name("Proc.Pause"), Some(OP_PROC_PAUSE));
        assert_eq!(op_by_name("Proc.Resume"), Some(OP_PROC_RESUME));
        assert_eq!(op_by_name("Vm.SnapshotHeld"), Some(OP_VM_SNAPSHOT_HELD));
        assert_eq!(op_by_name("Vm.SnapshotSelf"), Some(OP_VM_SNAPSHOT_SELF));
        assert_eq!(op_by_name("Vm.LoadSnapshot"), Some(OP_VM_LOAD_SNAPSHOT));
        assert_eq!(op_by_name("Vm.Restore"), Some(OP_VM_RESTORE));
        assert_eq!(op_by_name("Fs.Open"), Some(OP_FS_OPEN));
        assert_eq!(op_by_name("Fs.Close"), Some(OP_FS_CLOSE));
        assert_eq!(op_by_name("Vm.ResourceSame"), Some(OP_VM_RESOURCE_SAME));
        assert_eq!(op_by_name("Vm.DriveWait"), Some(OP_VM_DRIVE_WAIT));
        assert_eq!(op_by_name("Proc.RecvWait"), Some(OP_PROC_RECV_WAIT));
        assert_eq!(op_by_name("Wait.Wait"), Some(OP_WAIT_WAIT));
        assert_eq!(op_by_name("Wait.Choose"), Some(OP_WAIT_CHOOSE));
        assert_eq!(op_by_name("Wait.Cancel"), Some(OP_WAIT_CANCEL));
        assert_eq!(op_by_name("Dns.Resolve"), Some(OP_DNS_RESOLVE));
        assert_eq!(op_by_name("Tcp.Connect"), Some(OP_TCP_CONNECT));
        assert_eq!(op_by_name("Tcp.Close"), Some(OP_TCP_CLOSE));
        assert_eq!(op_by_name("Tls.Handshake"), Some(OP_TLS_HANDSHAKE));
        assert_eq!(op_by_name("Tls.Close"), Some(OP_TLS_CLOSE));
        assert_eq!(
            op_by_name("Vm.ServeTcpStream"),
            Some(OP_VM_SERVE_TCP_STREAM)
        );
        assert_eq!(
            op_by_name("Vm.ServeTlsStream"),
            Some(OP_VM_SERVE_TLS_STREAM)
        );
    }

    #[test]
    fn intrinsic_slots_match_the_constants() {
        assert_eq!(intrinsic_by_name("int.abs"), Some(INTRINSIC_INT_ABS));
        assert_eq!(intrinsic_by_name("int.add"), Some(INTRINSIC_INT_ADD));
        assert_eq!(intrinsic_by_name("bool.not"), Some(INTRINSIC_BOOL_NOT));
        assert_eq!(intrinsic(INTRINSIC_INT_ABS).reply, AbiType::INT);
    }

    #[test]
    fn intrinsic_identities_are_stable() {
        assert_eq!(
            intrinsic_identity(INTRINSIC_INT_ABS),
            intrinsic_identity(INTRINSIC_INT_ABS)
        );
        assert_eq!(intrinsic_manifest_digest(), intrinsic_manifest_digest());
    }

    #[test]
    fn names_round_trip() {
        for slot in 0..OP_COUNT {
            assert_eq!(op_by_name(&op_name(slot)), Some(slot));
        }
    }

    #[test]
    fn groups_resolve() {
        assert_eq!(group_by_name("Io"), Some(0));
        assert_eq!(group_by_name("Vm"), Some(6));
        assert_eq!(group_by_name("Nope"), None);
        assert_eq!(op_group(OP_CLOCK_NOW), group_by_name("Clock").unwrap());
        assert_eq!(group_by_name("Tcp.Stream"), Some(12));
        assert_eq!(group_by_name("Http.CleartextClient"), Some(16));
        assert_eq!(group_by_name("Tls.Stream"), Some(18));
        assert_eq!(group_by_name("Http.Client"), Some(20));
    }

    #[test]
    fn the_network_effect_sets_expand_transitively() {
        let stream = group_by_name("Tcp.Stream").unwrap();
        assert!(group_contains_op(stream, OP_TCP_READ));
        assert!(group_contains_op(stream, OP_TCP_CLOSE));
        assert!(!group_contains_op(stream, OP_TCP_CONNECT));
        assert!(!group_contains_op(stream, OP_TCP_LISTEN));

        let client = group_by_name("Tcp.Client").unwrap();
        assert!(group_contains_op(client, OP_TCP_CONNECT));
        assert!(group_contains_op(client, OP_TCP_READ));
        assert!(!group_contains_op(client, OP_TCP_LISTEN));

        let http = group_by_name("Http.CleartextClient").unwrap();
        assert!(group_contains_op(http, OP_DNS_RESOLVE));
        assert!(group_contains_op(http, OP_TCP_WRITE));
        assert!(!group_contains_op(http, OP_TCP_ACCEPT));

        let tls = group_by_name("Tls.Client").unwrap();
        assert!(group_contains_op(tls, OP_TLS_HANDSHAKE));
        assert!(group_contains_op(tls, OP_TLS_READ));
        assert!(!group_contains_op(tls, OP_TCP_CONNECT));

        let secure_http = group_by_name("Http.Client").unwrap();
        assert!(group_contains_op(secure_http, OP_DNS_RESOLVE));
        assert!(group_contains_op(secure_http, OP_TCP_CONNECT));
        assert!(group_contains_op(secure_http, OP_TLS_HANDSHAKE));
        assert!(group_contains_op(secure_http, OP_TLS_CLOSE));
        assert!(!group_contains_op(secure_http, OP_TCP_LISTEN));
    }

    #[test]
    fn normalized_rows_compare_exact_operation_closures() {
        assert!(row_name_included("Tcp.Stream", "Tcp.Client"));
        assert!(row_name_included("Tcp.Connect", "Tcp.Client"));
        assert!(!row_name_included("Tcp.Client", "Tcp.Stream"));
        assert!(!row_name_included("Tcp.Listen", "Tcp.Client"));
        assert!(row_name_included("Tls.Stream", "Tls.Client"));
        assert!(row_name_included("Tls.Handshake", "Http.Client"));
        assert!(row_name_included("Tcp.Client", "Http.Client"));
        assert!(!row_name_included("Http.Client", "Tls.Client"));
    }

    #[test]
    fn manifest_types_and_members_are_valid() {
        assert_eq!(validate_manifest(), Ok(()));
        assert_eq!(
            AbiType::RESULT_ACCEPT_NET_ERROR.text(),
            "Result[Pair[TcpStream, SocketAddress], NetError]"
        );
        assert!(AbiType::RESULT_ACCEPT_NET_ERROR.valid());
        assert!(!AbiType::Apply(AbiConstructor::Option, &[]).valid());
    }

    #[test]
    fn row_names_validate() {
        assert!(row_name_valid("Io"));
        assert!(row_name_valid("Io.Print"));
        assert!(row_name_valid("Fs"));
        assert!(row_name_valid("Vm.Run"));
        assert!(!row_name_valid("Io.Prin"));
        assert!(!row_name_valid("Web"));
        assert!(!row_name_valid("Web.Get"));
    }

    #[test]
    fn identities_are_distinct_and_stable_within_a_run() {
        let mut seen = Vec::new();
        for slot in 0..OP_COUNT {
            let id = op_identity(slot);
            assert!(!seen.contains(&id), "duplicate identity for slot {slot}");
            seen.push(id);
            assert_eq!(op_identity(slot), id);
        }
        assert_eq!(manifest_digest(), manifest_digest());
    }

    /// The snapshot classification is a semantic field, so it takes
    /// part in the operation identity and in the manifest digest.
    ///
    /// The classification decides whether a pending instance holds live
    /// host state, so a classification-only change is a behavior
    /// change. An identity that ignored it kept the manifest digest
    /// stable across that change.
    #[test]
    fn a_classification_only_change_moves_the_identity_and_the_manifest() {
        let slot = OP_CLOCK_NOW;
        let mut flipped = *op(slot);
        assert_eq!(flipped.snapshot, SnapshotClass::MachineState);
        flipped.snapshot = SnapshotClass::HostAttachment;
        let name = op_name(slot);
        assert_ne!(identity_of(&name, &flipped), op_identity(slot));
        let mutated: Vec<[u8; 32]> = (0..OP_COUNT)
            .map(|s| {
                if s == slot {
                    identity_of(&name, &flipped)
                } else {
                    op_identity(s)
                }
            })
            .collect();
        assert_ne!(manifest_digest_of(&mutated), manifest_digest());
        // Every other field of the definition is unchanged, so the
        // classification alone moved both digests.
        assert_eq!(flipped.params, op(slot).params);
        assert_eq!(flipped.reply, op(slot).reply);
        assert_eq!(flipped.schema, op(slot).schema);
    }

    /// Every operation declares one snapshot classification, and the
    /// suspending set is exactly the operations that reach live
    /// host state. A new operation must join this list on purpose.
    #[test]
    fn every_operation_declares_one_snapshot_class() {
        let suspending: Vec<String> = (0..OP_COUNT)
            .filter(|slot| op(*slot).suspends())
            .map(op_name)
            .collect();
        assert_eq!(
            suspending,
            vec![
                "Io.Print",
                "Io.Error",
                "Io.ReadLine",
                "Clock.Sleep",
                "Fs.Open",
                "Fs.Read",
                "Fs.Write",
                "Fs.Seek",
                "Fs.Flush",
                "Fs.Close",
                "Dns.Resolve",
                "Tcp.Connect",
                "Tcp.Listen",
                "Tcp.Accept",
                "Tcp.Read",
                "Tcp.Write",
                "Tcp.Shutdown",
                "Tcp.LocalAddress",
                "Tcp.PeerAddress",
                "Tcp.Close",
                "Tls.Handshake",
                "Tls.Read",
                "Tls.Write",
                "Tls.Shutdown",
                "Tls.LocalAddress",
                "Tls.PeerAddress",
                "Tls.Close",
            ]
        );
        // A VM control operation runs inside the driver loop, so it
        // never holds a host attachment.
        for slot in 0..OP_COUNT {
            if op(slot).kind == OpKind::VmControl {
                assert_eq!(op(slot).snapshot, SnapshotClass::MachineState);
            }
        }
    }

    #[test]
    fn fixed_member_excludes_vm_control() {
        assert_eq!(fixed_member("Io", "Print"), Some(OP_IO_PRINT));
        assert_eq!(fixed_member("Vm", "Run"), None);
        assert_eq!(fixed_member("Vm", "New"), None);
    }
}
