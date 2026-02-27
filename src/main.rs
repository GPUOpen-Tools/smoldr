// Copyright (c) Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Defines the parsed structure of a script file and contains the entrypoint to
//! run a script.
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::File;
use std::io::{BufReader, Read};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use std::{fmt, mem};

use bitflags::bitflags;
use clap::Parser;
use half::f16;
use index_vec::IndexVec;
use miette::{IntoDiagnostic, NamedSource, Result, SourceCode, WrapErr};
use num_traits::float::Float;
use tracing::{debug, info, trace, warn};

mod backend;
mod error;
mod parser;
#[cfg(test)]
mod tests;

use backend::{Backend, Continue};
use parser::{Identifier, SpanObj};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum DataType {
    U64,
    U32,
    U16,
    U8,
    F32,
    F16,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum IdentifierType {
    Source,
    Object,
    Blas,
    Tlas,
    Buffer,
    RootSig,
    ShaderId,
    ShaderTable,
    Pipeline,
    PipelineStateObject,
    View,
    CommandSignature,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum DumpDataType {
    DataType(DataType),
    /// Dump DXIL from shader object
    Dxil,
}

index_vec::define_index_type! {
    #[derive(Default)]
    struct SourceFileIdx = usize;
    DISPLAY_FORMAT = "{}";
}

index_vec::define_index_type! {
    #[derive(Default)]
    struct IdentifierIdx = usize;
    DISPLAY_FORMAT = "{}";
}

/// The backend-independent state and main entrypoint.
#[derive(Debug, Default)]
struct State {
    args: Args,

    /// [(filename, path, content)]
    ///
    /// filename is usually the same as the path.
    /// The only time it is different is when reading from stdin.
    /// Then filename is `"stdin"` and path is `""`.
    source_files: IndexVec<SourceFileIdx, (String, PathBuf, Arc<String>)>,
    identifiers: IndexVec<IdentifierIdx, (Identifier, IdentifierType)>,
    /// Map into identifiers list
    identifier_map: HashMap<String, IdentifierIdx>,

    /// HLSL source codes
    ///
    /// Map from identifier to content
    sources: HashMap<IdentifierIdx, String>,

    /// Cached buffers downloaded from the GPU.
    /// The cache is cleared when a dispatch is run.
    download_cache: HashMap<IdentifierIdx, Vec<u8>>,
}

/// Replace specific tokens in any code by "munching" the passed tokens bit by bit.
/// Idea and explanation: https://users.rust-lang.org/t/macro-to-replace-type-parameters/17903/2
macro_rules! replace_tokens {
    ($var:ident, $type:ident: $($input:tt)+) => {
        crate::replace_tokens!(@impl ($var, $type) (()) $($input)*)
    };

    // Opening brackets
    (@impl ($var:ident, $type:ident) ($($stack:tt)*) ($($first:tt)*) $($rest:tt)*) => {
        crate::replace_tokens!(@impl ($var, $type) (() $($stack)*) $($first)* __paren $($rest)*)
    };
    (@impl ($var:ident, $type:ident) ($($stack:tt)*) [$($first:tt)*] $($rest:tt)*) => {
        crate::replace_tokens!(@impl ($var, $type) (() $($stack)*) $($first)* __bracket $($rest)*)
    };
    (@impl ($var:ident, $type:ident) ($($stack:tt)*) {$($first:tt)*} $($rest:tt)*) => {
        crate::replace_tokens!(@impl ($var, $type) (() $($stack)*) $($first)* __brace $($rest)*)
    };

    // Close brackets
    (@impl ($var:ident, $type:ident) (($($close:tt)*) ($($top:tt)*) $($stack:tt)*) __paren $($rest:tt)*) => {
        crate::replace_tokens!(@impl ($var, $type) (($($top)* ($($close)*)) $($stack)*) $($rest)*)
    };
    (@impl ($var:ident, $type:ident) (($($close:tt)*) ($($top:tt)*) $($stack:tt)*) __bracket $($rest:tt)*) => {
        crate::replace_tokens!(@impl ($var, $type) (($($top)* [$($close)*]) $($stack)*) $($rest)*)
    };
    (@impl ($var:ident, $type:ident) (($($close:tt)*) ($($top:tt)*) $($stack:tt)*) __brace $($rest:tt)*) => {
        crate::replace_tokens!(@impl ($var, $type) (($($top)* {$($close)*}) $($stack)*) $($rest)*)
    };

    // Replace `VAR` token with `$var`.
    (@impl ($var:ident, $type:ident) (($($top:tt)*) $($stack:tt)*) VAR $($rest:tt)*) => {
        crate::replace_tokens!(@impl ($var, $type) (($($top)* $var) $($stack)*) $($rest)*)
    };

    // Replace `TYPE` token with `$type`.
    (@impl ($var:ident, $type:ident) (($($top:tt)*) $($stack:tt)*) TYPE $($rest:tt)*) => {
        crate::replace_tokens!(@impl ($var, $type) (($($top)* $type) $($stack)*) $($rest)*)
    };

    // No match, just accept the token
    (@impl ($var:ident, $type:ident) (($($top:tt)*) $($stack:tt)*) $first:tt $($rest:tt)*) => {
        crate::replace_tokens!(@impl ($var, $type) (($($top)* $first) $($stack)*) $($rest)*)
    };

    // Ready
    (@impl ($var:ident, $type:ident) (($($top:tt)+))) => {
        $($top)+
    };
}

/// Transform a match expression for all data types.
///
/// Change `VAR` to the enum variant of `DataType` and change `TYPE` to the primitive type.
macro_rules! all_data_types {
    (match $m:tt { $input:tt => $output:tt $(,)? $($pattern:pat => $result:expr,)* }) => {
        crate::all_data_types!(@impl ((U64, u64), (U32, u32), (U16, u16), (U8, u8), (F32, f32), (F16, f16))
            ($m) $input => $output, $($pattern => $result,)*)
    };

    (@impl ($(($var:ident, $type:ident)),+ $(,)?) ($m:tt) $input:tt => $output:tt, $($pattern:pat => $result:expr,)*) => {
        #[allow(unused_parens)]
        match $m {
            $(crate::replace_tokens!($var, $type: $input) => crate::replace_tokens!($var, $type: $output),)*
            $($pattern => $result,)*
        }
    };
}

pub(crate) use all_data_types;
pub(crate) use replace_tokens;

trait DataTypeTrait {
    /// May not be accurate, e.g. when converting a large number to an f16.
    fn from_usize(i: usize) -> Self;
}

/// A utility helper to avoid calling .into_diagnostic() on errors originating outside this crate.
trait ResultExt<T> {
    fn context<D: fmt::Display + Send + Sync + 'static>(self, msg: D) -> Result<T, miette::Report>;
    #[allow(dead_code)]
    fn with_context<D: fmt::Display + Send + Sync + 'static, F: FnOnce() -> D>(
        self, f: F,
    ) -> Result<T, miette::Report>;
}

// When adding something here, make sure to check that it matches the dx value in the assertions
// block in the dx12 backend
bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    struct GeometryConfig: u32 {
        const OPAQUE = 1;
        const NO_DUPLICATE_ANYHIT = 2;
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    struct AccelStructConfig: u32 {
        const ALLOW_UPDATE = 1;
        const ALLOW_COMPACTION = 2;
        const PREFER_FAST_TRACE = 4;
        const PREFER_FAST_BUILD = 8;
        const MINIMIZE_MEMORY = 16;
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    struct RootSigConfig: u32 {
        const ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT = 1;
        const DENY_VERTEX_SHADER_ROOT_ACCESS = 2;
        const DENY_HULL_SHADER_ROOT_ACCESS = 4;
        const DENY_DOMAIN_SHADER_ROOT_ACCESS = 8;
        const DENY_GEOMETRY_SHADER_ROOT_ACCESS = 16;
        const DENY_PIXEL_SHADER_ROOT_ACCESS = 32;
        const ALLOW_STREAM_OUTPUT = 64;
        const LOCAL_ROOT_SIGNATURE = 128;
        const DENY_AMPLIFICATION_SHADER_ROOT_ACCESS = 256;
        const DENY_MESH_SHADER_ROOT_ACCESS = 512;
        const CBV_SRV_UAV_HEAP_DIRECTLY_INDEXED = 1024;
        const SAMPLER_HEAP_DIRECTLY_INDEXED = 2048;
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    struct TlasBlasConfig: u32 {
        const TRIANGLE_CULL_DISABLE = 1;
        const TRIANGLE_FRONT_COUNTERCLOCKWISE = 2;
        const FORCE_OPAQUE = 4;
        const FORCE_NON_OPAQUE = 8;
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    struct StateObjectConfig: u32 {
        const LOCAL_DEP_ON_EXTERNAL = 1;
        const EXTERNAL_DEP_ON_LOCAL = 2;
        const ADD_TO_SO = 4;
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Fill {
    Series { from: Value, increment: Value },
    Const(Value),
}

#[derive(Clone, Debug, PartialEq)]
enum ValueContent {
    Typed { typ: DataType, element_count: u64, fill: Fill },
    Raw { values: Values },
}

#[derive(Clone, Debug, PartialEq)]
enum UnresolvedValueContent {
    Resolved(ValueContent),
    Raw { values: UnresolvedValues },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ViewType {
    /// Unordered access view aka read-write buffer
    Uav,
    /// Shader resource view aka read-only buffer
    Srv,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum DumpFormat {
    List,
    /// The format of EXPECT statements.
    Expect,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum InputViewType {
    Raw {
        typ: ViewType,
    },
    Typed {
        typ: ViewType,
        data_type: DataType,
    },
    Structured {
        typ: ViewType,
        /// Size of the struct in bytes
        struct_size: u32,
    },
    /// Raytracing acceleration structure
    RaytracingAccelStruct,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RootSigTable {
    typ: ViewType,
    register: u32,
    number: u32,
    space: u32,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RootSigView {
    typ: ViewType,
    register: u32,
    space: u32,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RootSigConst {
    number: u32,
    register: u32,
    space: u32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum PipelineType {
    Compute,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum PipelineStateObjectType {
    Pipeline,
    Collection,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Bind {
    index: u32,
    view: Identifier,
}

#[derive(Clone, Debug, PartialEq)]
struct RootValConst {
    index: u32,
    content: ValueContent,
}

#[derive(Clone, Debug, PartialEq)]
struct UnresolvedRootValConst {
    index: u32,
    content: UnresolvedValueContent,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RootValView {
    index: u32,
    /// Always set for global root signatures, not set for local root signatures.
    typ: Option<ViewType>,
    buffer: Identifier,
}

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
#[repr(transparent)]
struct Dim3([u32; 3]);

#[derive(Clone, Debug, Default, PartialEq)]
#[repr(transparent)]
struct Vertex([f32; 3]);

/// 4x3 transformation matrix, stored row-major. (`float[3][4]` in C++)
#[derive(Clone, Debug, Default, PartialEq)]
#[repr(transparent)]
struct Transform([f32; 12]);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Value {
    U64(u64),
    U32(u32),
    U16(u16),
    U8(u8),
    F32(f32),
    F16(f16),
}

/// A value whose exact value is unknown.
///
/// The value is either a constant or the address of a buffer.
/// The address of the buffer is only known after the buffer is created, at that point in can be
/// resolved to a constant `Value`.
#[derive(Clone, Debug, PartialEq)]
enum UnresolvedValue {
    Resolved(Value),
    /// 64-bit address of a buffer
    Gpuva(Identifier),
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Values {
    data: Vec<Value>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct UnresolvedValues {
    data: Vec<UnresolvedValue>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum RootSigEntry {
    Table(RootSigTable),
    /// UAV or SRV
    View(RootSigView),
    Const(RootSigConst),
}

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
enum ShaderReference {
    #[default]
    None,
    Name(Identifier),
    ShaderId(Identifier),
}

#[derive(Clone, Debug, PartialEq)]
struct UnresolvedShaderTableRecord {
    index: u32,
    shader: ShaderReference,
    root_val: UnresolvedRootVal,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct UnresolvedRootVal {
    binds: Vec<Bind>,
    consts: Vec<UnresolvedRootValConst>,
    views: Vec<RootValView>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct RootVal {
    binds: Vec<IdentifierIdx>,
    consts: Vec<RootValConst>,
    views: Vec<IdentifierIdx>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Export {
    name: String,
    to_rename: Option<String>,
}

/// Library or collection with associated exports
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LinkObject {
    name: Identifier,
    exports: Vec<Export>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct HitGroup {
    name: Identifier,
    shaders: [Option<Identifier>; 3],
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Aabb {
    min: Vertex,
    max: Vertex,
}

/// Procedural geometry in a blas.
#[derive(Clone, Debug, Default, PartialEq)]
struct ProceduralGeometry {
    aabbs: Vec<Aabb>,
    config: GeometryConfig,
}

/// Triangle geometry in a blas.
#[derive(Clone, Debug, Default, PartialEq)]
struct TriangleGeometry {
    vertices: Vec<Vertex>,
    transform: Option<Transform>,
    config: GeometryConfig,
}

/// Blas reference in a tlas.
#[derive(Clone, Debug, Default, PartialEq)]
struct TlasBlas {
    name: Identifier,
    id: Option<u32>,
    mask: Option<u8>,
    index_contrib: Option<u32>,
    transform: Option<Transform>,
    config: TlasBlasConfig,
}

/// Different types of dispatches
#[derive(Clone, Debug, PartialEq)]
enum DispatchType {
    /// Compute dispatch
    Dispatch { dimensions: Dim3 },
    DispatchRays {
        dimensions: Dim3,
        /// The tables are RayGen, Miss, HitGroup, Callable.
        tables: Box<[Option<Identifier>; 4]>,
    },
    ExecuteIndirect {
        signature: Identifier,
        argument_buffer: Identifier,
        argument_offset: Option<u64>,
        count_buffer: Option<Identifier>,
        count_offset: Option<u64>,
        max_commands: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum PipelineKind {
    Pipeline,
    PipelineStateObject,
}

#[derive(Clone, Debug, PartialEq)]
enum DispatchContent<'a> {
    Dispatch,
    DispatchRays {
        tables: &'a [Option<IdentifierIdx>],
    },
    ExecuteIndirect {
        signature: IdentifierIdx,
        pipeline_kind: PipelineKind,
        argument_buffer: IdentifierIdx,
        count_buffer: Option<IdentifierIdx>,
    },
}

#[derive(Clone, Debug, PartialEq)]
enum CommandSignatureArgument {
    Constant {
        index: u32,
        /// In 32 bit values
        number: u32,
        /// In bytes, must be a multiple of 4
        offset: u32,
    },
    View {
        typ: ViewType,
        index: u32,
    },
    Dispatch,
    DispatchRays,
}

/// A directive in a script file
#[derive(Clone, Debug, PartialEq)]
enum Directive {
    Source {
        name: Identifier,
        content: String,
    },
    Object {
        name: Identifier,
        source: Identifier,
        shader_model: String,
        entrypoint: Option<String>,
        args: Vec<String>,
    },
    ObjectDxil {
        name: Identifier,
        content: String,
    },
    Blas {
        name: Identifier,
        procedurals: Vec<ProceduralGeometry>,
        triangles: Vec<TriangleGeometry>,
        config: AccelStructConfig,
    },
    Tlas {
        name: Identifier,
        blas: Vec<TlasBlas>,
        config: AccelStructConfig,
    },
    Buffer {
        name: Identifier,
        content: UnresolvedValueContent,
    },
    RootSig {
        name: Identifier,
        entries: Vec<RootSigEntry>,
        config: RootSigConfig,
    },
    RootSigDxil {
        name: Identifier,
        object: Identifier,
    },
    ShaderId {
        name: Identifier,
        pipeline_state_object: Identifier,
        shader_name: Identifier,
    },
    ShaderTable {
        name: Identifier,
        pipeline_state_object: Identifier,
        records: Vec<UnresolvedShaderTableRecord>,
    },
    Pipeline {
        name: Identifier,
        typ: PipelineType,
        shaders: Vec<Identifier>,
        /// Root signature
        root_sig: Option<Identifier>,
    },
    PipelineStateObject {
        name: Identifier,
        typ: PipelineStateObjectType,
        add_to: Option<Identifier>,
        libs: Vec<LinkObject>,
        collections: Vec<LinkObject>,
        hit_groups: Vec<HitGroup>,
        config: StateObjectConfig,
    },
    View {
        name: Identifier,
        /// The buffer can be null for raytracing acceleration structures
        buffer: Option<Identifier>,
        typ: InputViewType,
    },
    CommandSignature {
        name: Identifier,
        stride: Option<u32>,
        arguments: Vec<CommandSignatureArgument>,
        root_sig: Option<Identifier>,
    },
    Dispatch {
        identifier: Identifier,
        pipeline: Identifier,
        root_val: UnresolvedRootVal,
        root_sig: Option<Identifier>,
        typ: DispatchType,
    },
    Include {
        identifier: Identifier,
        path: Identifier,
    },
    Sleep {
        identifier: Identifier,
        duration: SpanObj<Duration>,
    },
    Dump {
        identifier: Identifier,
        resource: Identifier,
        typ: DumpDataType,
        print_stride: Option<NonZeroU64>,
        format: DumpFormat,
    },
    Expect {
        identifier: Identifier,
        resource: Identifier,
        epsilon: Option<SpanObj<Value>>,
        offset: SpanObj<u64>,
        values: UnresolvedValues,
        value_spans: Vec<Identifier>,
    },
    AssertShaderId {
        identifier: Identifier,
        // `true` for equal, `false` for not equal
        equal: bool,
        id_a: Identifier,
        id_b: Identifier,
    },
}

#[derive(Parser, Clone, Debug, Default)]
#[command(version, about)]
struct Args {
    /// The script to run
    filename: Option<PathBuf>,
    #[arg(long, default_value_t = Default::default(), value_enum)]
    backend: backend::BackendType,
    /// Choose a device by index
    #[arg(short, long, default_value_t = 0)]
    device: u32,
    /// List all available devices
    #[arg(short, long)]
    list_devices: bool,
    /// Enable validation layers
    #[arg(long)]
    validate: bool,
    /// When validation layers are enabled, disable GPU validation layers
    #[arg(long)]
    no_gpu_validate: bool,
    /// Ignore EXPECT statements where the data does not match
    #[arg(long)]
    ignore_expect: bool,
    /// Show a window and execute the script once per frame
    ///
    /// Can be used for capturing tools
    #[arg(long)]
    window: bool,
    /// How often to repeat the script
    ///
    /// When `window` is enabled, this defaults to zero, meaning infinite frames.
    #[arg(long)]
    repeat: Option<u64>,
}

impl fmt::Display for IdentifierType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", match self {
            Self::Source => "SOURCE",
            Self::Object => "OBJECT",
            Self::Blas => "BLAS",
            Self::Tlas => "TLAS",
            Self::Buffer => "BUFFER",
            Self::RootSig => "ROOT",
            Self::ShaderId => "SHADERID",
            Self::ShaderTable => "SHADERTABLE",
            Self::Pipeline => "PIPELINE",
            Self::PipelineStateObject => "PSO",
            Self::View => "VIEW",
            Self::CommandSignature => "COMMAND_SIGNATURE",
        })
    }
}

macro_rules! data_type_impl {
    ($ty:ident, $dty:expr) => {
        impl DataTypeTrait for $ty {
            fn from_usize(i: usize) -> Self { i as Self }
        }
    };
}

data_type_impl!(u64, DataType::U64);
data_type_impl!(u32, DataType::U32);
data_type_impl!(u16, DataType::U16);
data_type_impl!(u8, DataType::U8);
data_type_impl!(f32, DataType::F32);

impl DataTypeTrait for f16 {
    fn from_usize(i: usize) -> Self { f16::from_f32(i as f32) }
}

impl<T, E> ResultExt<T> for Result<T, E>
where Result<T, E>: IntoDiagnostic<T, E>
{
    fn context<D: fmt::Display + Send + Sync + 'static>(self, msg: D) -> Result<T, miette::Report> {
        self.into_diagnostic().context(msg)
    }

    fn with_context<D: fmt::Display + Send + Sync + 'static, F: FnOnce() -> D>(
        self, f: F,
    ) -> Result<T, miette::Report> {
        self.into_diagnostic().with_context(f)
    }
}

impl Fill {
    #[allow(dead_code)]
    fn get_type(&self) -> DataType {
        match self {
            Self::Series { from, .. } => from.get_type(),
            Self::Const(val) => val.get_type(),
        }
    }

    fn fill(&self, data: &mut [u8]) {
        match self {
            Self::Series { from, increment } => {
                all_data_types!(match (from, increment) {
                    (Value::VAR(f), Value::VAR(inc)) => {
                        assert_eq!(
                            data.len() % mem::size_of::<TYPE>(),
                            0,
                            "Buffer size must be a multiple of the element size"
                        );
                        for (i, c) in data.chunks_mut(mem::size_of::<TYPE>()).enumerate() {
                            let o = f + inc * TYPE::from_usize(i);
                            //let o = from + increment * i as usize;
                            c.copy_from_slice(o.to_le_bytes().as_ref());
                        }
                    }
                    _ => unreachable!("Inconsistent type in from and increment in series"),
                });
            }
            Self::Const(val) => {
                let val_data = val.to_data();
                assert_eq!(
                    data.len() % val_data.len(),
                    0,
                    "Buffer size must be a multiple of the element size"
                );
                for c in data.chunks_mut(val_data.len()) {
                    c.copy_from_slice(&val_data);
                }
            }
        }
    }
}

impl Value {
    fn from_data(typ: DataType, data: &[u8]) -> Self {
        assert_eq!(
            data.len(),
            typ.len(),
            "Buffer size {} must be a multiple of the element size {}",
            data.len(),
            typ.len(),
        );

        all_data_types!(match typ {
            (DataType::VAR) => {
                Self::VAR(TYPE::from_le_bytes(data.try_into().unwrap()))
            }
        })
    }

    fn to_data(self) -> Vec<u8> {
        let res: Vec<u8> = {
            all_data_types!(match self {
                (Self::VAR(v)) => (v.to_le_bytes().into()),
            })
        };
        debug_assert_eq!(res.len(), self.len());
        res
    }

    fn get_type(&self) -> DataType {
        all_data_types!(match self {
            (Self::VAR(_)) => (DataType::VAR),
        })
    }

    fn len(&self) -> usize { self.get_type().len() }

    /// Compares two values lists, allowing a epsilon of the same type.
    ///
    /// Returns `true` if the values are close or `false` if there is a difference larger than
    /// `epsilon`.
    fn compare_epsilon(self, other: Value, epsilon: Value) -> bool {
        assert_eq!(self.get_type(), other.get_type());
        assert_eq!(self.get_type(), epsilon.get_type());

        // See https://floating-point-gui.de/errors/comparison/ for a list of edge cases.
        if self == other {
            return true;
        }

        match (self, other, epsilon) {
            (Self::U32(s), Self::U32(o), Self::U32(d)) => {
                if s < o {
                    (o - s) <= d
                } else {
                    (s - o) <= d
                }
            }
            (Self::U16(s), Self::U16(o), Self::U16(d)) => {
                if s < o {
                    (o - s) <= d
                } else {
                    (s - o) <= d
                }
            }
            (Self::U8(s), Self::U8(o), Self::U8(d)) => {
                if s < o {
                    (o - s) <= d
                } else {
                    (s - o) <= d
                }
            }
            (Self::F32(s), Self::F32(o), Self::F32(d)) => (s - o).abs() <= d,
            (Self::F16(s), Self::F16(o), Self::F16(d)) => (s - o).abs() <= d,
            _ => panic!("Unsupported data type for epsilon comparison"),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        all_data_types!(match self {
            (Value::VAR(v)) => (v.fmt(f)),
        })
    }
}

impl UnresolvedValue {
    fn get_type(&self) -> DataType {
        match self {
            Self::Resolved(v) => v.get_type(),
            Self::Gpuva(_) => DataType::U64,
        }
    }

    fn len(&self) -> usize { self.get_type().len() }
}

impl fmt::Display for UnresolvedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resolved(v) => write!(f, "{v}"),
            Self::Gpuva(i) => write!(f, "GPUVA {i}"),
        }
    }
}

impl Values {
    fn from_data(typ: DataType, data: &[u8]) -> Self {
        assert_eq!(
            data.len() % typ.len(),
            0,
            "Buffer size {} must be a multiple of the element size {}",
            data.len(),
            typ.len(),
        );

        let mut values = Vec::new();
        for c in data.chunks(typ.len()) {
            values.push(Value::from_data(typ, c));
        }
        Self { data: values }
    }

    fn from_data_using_types(data: &[u8], use_types: &Values) -> Self {
        assert_eq!(data.len(), use_types.byte_len(), "Sizes have to match");

        let mut values = Vec::new();
        let mut pos = 0;
        for t in &use_types.data {
            let t = t.get_type();
            let len = t.len();
            values.push(Value::from_data(t, &data[pos..(pos + len)]));
            pos += len;
        }
        Self { data: values }
    }

    fn byte_len(&self) -> usize { self.data.iter().map(|v| v.len()).sum() }

    fn fill(&self, data: &mut [u8]) {
        assert_eq!(data.len(), self.byte_len(), "Length mismatch to fill buffer");

        let mut pos = 0;
        for v in &self.data {
            all_data_types!(match v {
                (Value::VAR(v)) => {
                    let len = mem::size_of::<TYPE>();
                    data[pos..(pos + len)].copy_from_slice(&v.to_le_bytes());
                    pos += len;
                }
            })
        }
        debug_assert_eq!(pos, data.len());
    }

    fn get_data(&self) -> Vec<u8> {
        let mut res = vec![0; self.byte_len()];
        self.fill(&mut res);
        res
    }

    /// Compares two value lists, allowing a epsilon of the same type.
    ///
    /// Returns `Ok` on success or the index of the first failing element if there is a difference
    /// larger than `epsilon`.
    fn compare_epsilon(&self, other: &Values, epsilon: Value) -> Result<(), usize> {
        assert_eq!(self.data.len(), other.data.len());

        for (i, (v, o)) in self.data.iter().zip(other.data.iter()).enumerate() {
            let is_equal = v.compare_epsilon(*o, epsilon);
            if !is_equal {
                return Err(i);
            }
        }
        Ok(())
    }
}

impl fmt::Display for Values {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            write!(f, "{:?}", self.data)
        } else {
            if let Some(v) = self.data.first() {
                v.fmt(f)?;
            }
            for v in self.data.iter().skip(1) {
                write!(f, " ")?;
                v.fmt(f)?;
            }
            Ok(())
        }
    }
}

impl UnresolvedValues {
    fn byte_len(&self) -> usize { self.data.iter().map(|v| v.len()).sum() }

    fn resolve(&self, state: &State, backend: &dyn Backend) -> Result<Values> {
        let data = self
            .data
            .iter()
            .map(|v| match v {
                UnresolvedValue::Resolved(v) => Ok(*v),
                UnresolvedValue::Gpuva(name) => {
                    Ok(Value::U64(state.get_gpu_address(backend, name)?))
                }
            })
            .collect::<Result<_>>()?;
        Ok(Values { data })
    }
}

impl fmt::Display for UnresolvedValues {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            write!(f, "{:?}", self.data)
        } else {
            if let Some(v) = self.data.first() {
                write!(f, "{v}")?;
            }
            for v in self.data.iter().skip(1) {
                write!(f, " {v}")?;
            }
            Ok(())
        }
    }
}

impl DataType {
    fn len(&self) -> usize {
        all_data_types!(match self {
            (Self::VAR) => (mem::size_of::<TYPE>()),
        })
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = match self {
            Self::U64 => "uint64",
            Self::U32 => "uint32",
            Self::U16 => "uint16",
            Self::U8 => "uint8",
            Self::F32 => "float",
            Self::F16 => "float16",
        };
        write!(f, "{n}")
    }
}

impl ValueContent {
    fn len(&self) -> usize {
        match self {
            Self::Typed { typ, element_count, .. } => {
                typ.len() * usize::try_from(*element_count).unwrap()
            }
            Self::Raw { values } => values.byte_len(),
        }
    }

    fn fill(&self, data: &mut [u8]) {
        assert_eq!(data.len(), self.len(), "Length mismatch to fill buffer");
        match self {
            Self::Typed { fill, .. } => {
                fill.fill(data);
            }
            Self::Raw { values } => {
                values.fill(data);
            }
        }
    }
}

impl UnresolvedValueContent {
    fn len(&self) -> usize {
        match self {
            Self::Resolved(v) => v.len(),
            Self::Raw { values } => values.byte_len(),
        }
    }

    fn resolve(&self, state: &State, backend: &dyn Backend) -> Result<ValueContent> {
        match self {
            Self::Resolved(c) => Ok(c.clone()),
            Self::Raw { values } => {
                Ok(ValueContent::Raw { values: values.resolve(state, backend)? })
            }
        }
    }
}

impl UnresolvedRootValConst {
    fn resolve(&self, state: &State, backend: &dyn Backend) -> Result<RootValConst> {
        Ok(RootValConst { index: self.index, content: self.content.resolve(state, backend)? })
    }
}

impl UnresolvedRootVal {
    fn resolve(&self, state: &State, backend: &dyn Backend) -> Result<RootVal> {
        let binds = self
            .binds
            .iter()
            .map(|b| state.get_type(&b.view, IdentifierType::View))
            .collect::<Result<Vec<_>>>()?;
        let consts =
            self.consts.iter().map(|c| c.resolve(state, backend)).collect::<Result<Vec<_>>>()?;
        let views = self
            .views
            .iter()
            .map(|v| state.get_type(&v.buffer, IdentifierType::Buffer))
            .collect::<Result<Vec<_>>>()?;
        Ok(RootVal { binds, consts, views })
    }
}

impl Directive {
    fn get_identifier(&self) -> &Identifier {
        match self {
            Self::Source { name, .. }
            | Self::Object { name, .. }
            | Self::ObjectDxil { name, .. }
            | Self::Blas { name, .. }
            | Self::Tlas { name, .. }
            | Self::Buffer { name, .. }
            | Self::RootSig { name, .. }
            | Self::RootSigDxil { name, .. }
            | Self::ShaderId { name, .. }
            | Self::ShaderTable { name, .. }
            | Self::Pipeline { name, .. }
            | Self::PipelineStateObject { name, .. }
            | Self::View { name, .. }
            | Self::CommandSignature { name, .. } => name,
            Self::Dispatch { identifier, .. }
            | Self::Include { identifier, .. }
            | Self::Sleep { identifier, .. }
            | Self::Dump { identifier, .. }
            | Self::Expect { identifier, .. }
            | Self::AssertShaderId { identifier, .. } => identifier,
        }
    }
}

impl State {
    fn new(args: Args) -> Self {
        State {
            args,
            source_files: Default::default(),
            identifiers: Default::default(),
            identifier_map: Default::default(),
            sources: Default::default(),
            download_cache: Default::default(),
        }
    }

    fn reset(&mut self) {
        self.identifiers.clear();
        self.identifier_map.clear();
        self.sources.clear();
        self.download_cache.clear();
    }

    fn get_named_source(&self, i: SourceFileIdx) -> NamedSource<Arc<String>> {
        self.source_files
            .get(i)
            .map(|(n, _, c)| NamedSource::new(n.clone(), c.clone()))
            .unwrap_or_else(|| {
                warn!("Failed to find source file {i}");
                NamedSource::new("unknown", Default::default())
            })
    }

    /// Add to identifier list, make sure it is unique and return its index.
    fn add_identifier(&mut self, i: Identifier, ty: IdentifierType) -> Result<IdentifierIdx> {
        // NULL is not a valid identifier
        if i.content == "NULL" {
            return Err(error::NullIdentifier { name: i }.into());
        }

        match self.identifier_map.entry(i.content.clone()) {
            Entry::Occupied(e) => {
                let name0 = self.identifiers[*e.get()].0.clone();
                Err(error::DuplicateIdentifier { name0, name1: i }.into())
            }
            Entry::Vacant(e) => {
                let num = IdentifierIdx::new(self.identifiers.len());
                self.identifiers.push((i, ty));
                e.insert(num);
                Ok(num)
            }
        }
    }

    fn get_identifier(&self, i: &Identifier) -> Result<IdentifierIdx> {
        self.identifier_map
            .get(&i.content)
            .copied()
            .ok_or_else(|| error::UnknownIdentifier { name: i.clone() }.into())
    }

    fn get_type(&self, i: &Identifier, typ: IdentifierType) -> Result<IdentifierIdx> {
        let n = self.get_identifier(i)?;
        let actual_typ = self.identifiers[n].1;
        if actual_typ == typ {
            Ok(n)
        } else {
            Err(error::WrongType {
                expected: vec![typ],
                actual: self.identifiers[n].1,
                declaration: self.identifiers[n].0.clone(),
                used: i.clone(),
            }
            .into())
        }
    }

    fn download(&mut self, backend: &mut dyn Backend, buffer: IdentifierIdx) -> Result<&[u8]> {
        match self.download_cache.entry(buffer) {
            Entry::Occupied(e) => Ok(e.into_mut().as_slice()),
            Entry::Vacant(e) => {
                let data = backend.download(buffer)?;
                Ok(e.insert(data).as_slice())
            }
        }
    }

    fn get_gpu_address(&self, backend: &dyn Backend, buffer: &Identifier) -> Result<u64> {
        let n = self.get_identifier(buffer)?;
        let actual_type = self.identifiers[n].1;
        const ALLOWED_TYPES: &[IdentifierType] = &[
            IdentifierType::Buffer,
            IdentifierType::ShaderTable,
            IdentifierType::Tlas,
            IdentifierType::Blas,
        ];
        if !ALLOWED_TYPES.contains(&actual_type) {
            return Err(error::WrongType {
                expected: ALLOWED_TYPES.to_vec(),
                actual: actual_type,
                declaration: self.identifiers[n].0.clone(),
                used: buffer.clone(),
            }
            .into());
        }
        backend.get_gpuva(n, actual_type)
    }

    /// Open and parse the file at `path`.
    fn parse_file(&mut self, path: &Path) -> Result<Vec<Directive>> {
        let mut file_reader = BufReader::new(
            File::open(path)
                .context(format!("Input file: {}", path.display()))
                .wrap_err("Failed to open input file")?,
        );
        self.parse_stream(path.display().to_string(), path, &mut file_reader)
    }

    /// Parse a file from a stream.
    fn parse_stream(
        &mut self, name: String, path: &Path, stream: &mut dyn Read,
    ) -> Result<Vec<Directive>> {
        let mut input = String::new();
        stream.read_to_string(&mut input).context(format!("Read file: {}", path.display()))?;
        let input = Arc::new(input);
        self.source_files.push((name, path.to_owned(), input.clone()));
        parser::parse(&input, self, SourceFileIdx::new(self.source_files.len() - 1))
    }

    /// Run a list of statements and print warnings afterwards.
    fn run(&mut self, backend: &mut dyn Backend, dirs: &[Directive]) -> Result<()> {
        self.apply_directives(&mut *backend, dirs)?;

        let msgs = backend.messages().unwrap_or_else(|e| e.to_string());
        if !msgs.is_empty() {
            warn!(messages = %msgs);
        }
        Ok(())
    }

    fn apply_directives(&mut self, backend: &mut dyn Backend, dirs: &[Directive]) -> Result<()> {
        // Errors from failed expect statements do not cause immediate test exit.
        let mut expect_errors = vec![];

        // Return single error directly or wrap
        let expect_err_result = |mut expect_errors: Vec<_>| {
            if expect_errors.is_empty() {
                Ok(())
            } else if expect_errors.len() == 1 {
                Err(expect_errors.pop().unwrap())
            } else {
                Err(error::Failure { errors: expect_errors }.into())
            }
        };

        for d in dirs {
            trace!(statement = ?d, "Run statement");

            let is_expect = matches!(d, Directive::Expect { .. });

            if !is_expect {
                // Return error if earlier EXPECT statements failed and we would run a different
                // statement now.
                expect_err_result(mem::take(&mut expect_errors))?;
            }

            let result = self.apply(backend, d).map_err(|mut e| {
                if e.code().is_none() {
                    // If this is no custom error, wrap it
                    if self.args.validate {
                        e = error::ApplyDirectiveValidation {
                            cause: e,
                            declaration: d.get_identifier().clone(),
                        }
                        .into();
                    } else {
                        e = error::ApplyDirective {
                            cause: e,
                            declaration: d.get_identifier().clone(),
                        }
                        .into();
                    }
                }

                if e.source_code().is_none() {
                    // Attach source code if there is not already one
                    e = e.with_source_code(self.get_named_source(d.get_identifier().source_file));
                }
                e
            });

            // Fetch validation layer messages
            let msgs = backend.messages().unwrap_or_else(|e| e.to_string());
            if !msgs.is_empty() {
                if let Err(e) = result {
                    return Err(e.context(msgs));
                }

                warn!(messages = %msgs);
            }

            if let Err(e) = result {
                if !is_expect {
                    return Err(e);
                } else {
                    expect_errors.push(e);
                    // Abort testing if we have too many failures to keep the log in check
                    if expect_errors.len() >= 32 {
                        return Err(error::Abort { errors: expect_errors }.into());
                    }
                }
            }
        }
        expect_err_result(expect_errors)
    }

    fn apply(&mut self, backend: &mut dyn Backend, dir: &Directive) -> Result<()> {
        match dir {
            Directive::Source { name, content } => {
                let id = self.add_identifier(name.clone(), IdentifierType::Source)?;
                self.sources.insert(id, content.clone());
            }
            Directive::Object { name, source, .. } => {
                let id = self.add_identifier(name.clone(), IdentifierType::Object)?;
                let source = self.get_type(source, IdentifierType::Source)?;
                let source = self.sources[&source].as_str();
                let cur_path = &self.source_files[name.source_file].1;
                backend.compile(id, source, cur_path, dir)?;
            }
            Directive::ObjectDxil { name, content } => {
                let id = self.add_identifier(name.clone(), IdentifierType::Object)?;
                backend.compile_dxil(id, content, dir)?;
            }
            Directive::Blas { name, .. } => {
                let id = self.add_identifier(name.clone(), IdentifierType::Blas)?;
                backend.create_blas(id, dir)?;
            }
            Directive::Tlas { name, blas, .. } => {
                let id = self.add_identifier(name.clone(), IdentifierType::Tlas)?;
                let blas = blas
                    .iter()
                    .map(|b| self.get_type(&b.name, IdentifierType::Blas))
                    .collect::<Result<Vec<_>>>()?;
                backend.create_tlas(id, &blas, dir)?;
            }
            Directive::Buffer { name, content, .. } => {
                let id = self.add_identifier(name.clone(), IdentifierType::Buffer)?;
                let content = content.resolve(self, backend)?;
                backend.create_buffer(id, dir)?;
                backend.upload(id, &mut |data| content.fill(data))?;
            }
            Directive::RootSig { name, .. } => {
                let id = self.add_identifier(name.clone(), IdentifierType::RootSig)?;
                backend.create_root_sig(id, dir)?;
            }
            Directive::RootSigDxil { name, object } => {
                let id = self.add_identifier(name.clone(), IdentifierType::RootSig)?;
                let object = self.get_type(object, IdentifierType::Object)?;
                backend.create_root_sig_dxil(id, object, dir)?;
            }
            Directive::ShaderId { name, pipeline_state_object, .. } => {
                let id = self.add_identifier(name.clone(), IdentifierType::ShaderId)?;
                let pso =
                    self.get_type(pipeline_state_object, IdentifierType::PipelineStateObject)?;
                backend.create_shader_id(id, pso, dir)?;
            }
            Directive::ShaderTable { name, pipeline_state_object, records } => {
                let id = self.add_identifier(name.clone(), IdentifierType::ShaderTable)?;
                let pso =
                    self.get_type(pipeline_state_object, IdentifierType::PipelineStateObject)?;
                let root_val_ids = records
                    .iter()
                    .map(|r| r.root_val.resolve(self, backend))
                    .collect::<Result<Vec<_>>>()?;
                let shaders = records
                    .iter()
                    .map(|r| {
                        if let ShaderReference::ShaderId(id) = &r.shader {
                            Ok(Some(self.get_type(id, IdentifierType::ShaderId)?))
                        } else {
                            Ok(None)
                        }
                    })
                    .collect::<Result<Vec<_>>>()?;

                backend.create_shader_table(id, pso, &root_val_ids, &shaders, dir)?;
            }
            Directive::Pipeline { name, typ, shaders, root_sig } => {
                let id = self.add_identifier(name.clone(), IdentifierType::Pipeline)?;
                assert_eq!(*typ, PipelineType::Compute, "Unexpected pipeline type");
                let shaders = shaders
                    .iter()
                    .map(|s| self.get_type(s, IdentifierType::Object))
                    .collect::<Result<Vec<_>>>()?;

                if shaders.len() != 1 {
                    return Err(error::InvalidShaderCount {
                        name: name.clone(),
                        shader_count: shaders.len(),
                    }
                    .into());
                }

                let root_sig = root_sig
                    .as_ref()
                    .map(|r| self.get_type(r, IdentifierType::RootSig))
                    .transpose()?;
                backend.create_compute_pipeline(id, shaders[0], root_sig, dir)?;
            }
            Directive::PipelineStateObject { name, add_to, libs, collections, .. } => {
                let id = self.add_identifier(name.clone(), IdentifierType::PipelineStateObject)?;
                let add_to = add_to
                    .as_ref()
                    .map(|r| self.get_type(r, IdentifierType::PipelineStateObject))
                    .transpose()?;
                let libs = libs
                    .iter()
                    .map(|s| self.get_type(&s.name, IdentifierType::Object))
                    .collect::<Result<Vec<_>>>()?;
                let cols = collections
                    .iter()
                    .map(|s| self.get_type(&s.name, IdentifierType::PipelineStateObject))
                    .collect::<Result<Vec<_>>>()?;
                backend.create_pipeline_state_object(id, add_to, &libs, &cols, dir)?;
            }
            Directive::View { name, buffer, typ, .. } => {
                let id = self.add_identifier(name.clone(), IdentifierType::View)?;
                let buf_type = if *typ == InputViewType::RaytracingAccelStruct {
                    IdentifierType::Tlas
                } else {
                    if buffer.is_none() {
                        return Err(error::ViewNoBuffer { name: name.clone() }.into());
                    }
                    IdentifierType::Buffer
                };
                let buffer = buffer.as_ref().map(|b| self.get_type(b, buf_type)).transpose()?;
                backend.create_view(id, buffer, dir)?;
            }
            Directive::CommandSignature { name, root_sig, .. } => {
                let id = self.add_identifier(name.clone(), IdentifierType::CommandSignature)?;
                let root_sig = root_sig
                    .as_ref()
                    .map(|b| self.get_type(b, IdentifierType::RootSig))
                    .transpose()?;
                backend.create_command_signature(id, root_sig, dir)?;
            }
            Directive::Dispatch { identifier, pipeline, root_val, root_sig, typ, .. } => {
                let content_tables;
                let (pipeline_type, content) = match typ {
                    DispatchType::Dispatch { .. } => {
                        (IdentifierType::Pipeline, DispatchContent::Dispatch)
                    }
                    DispatchType::DispatchRays { tables, .. } => {
                        content_tables = tables
                            .iter()
                            .map(|t| {
                                t.as_ref()
                                    .map(|t| self.get_type(t, IdentifierType::ShaderTable))
                                    .transpose()
                            })
                            .collect::<Result<Vec<_>>>()?;
                        (IdentifierType::PipelineStateObject, DispatchContent::DispatchRays {
                            tables: &content_tables,
                        })
                    }
                    DispatchType::ExecuteIndirect {
                        signature,
                        argument_buffer,
                        count_buffer,
                        ..
                    } => {
                        // Both, PSOs and pipelines are supported
                        let n = self.get_identifier(pipeline)?;
                        let actual_type = self.identifiers[n].1;
                        let pipeline_kind = if actual_type == IdentifierType::Pipeline {
                            PipelineKind::Pipeline
                        } else if actual_type == IdentifierType::PipelineStateObject {
                            PipelineKind::PipelineStateObject
                        } else {
                            return Err(error::WrongType {
                                expected: vec![
                                    IdentifierType::Pipeline,
                                    IdentifierType::PipelineStateObject,
                                ],
                                actual: actual_type,
                                declaration: self.identifiers[n].0.clone(),
                                used: pipeline.clone(),
                            }
                            .into());
                        };

                        let signature =
                            self.get_type(signature, IdentifierType::CommandSignature)?;
                        let argument_buffer =
                            self.get_type(argument_buffer, IdentifierType::Buffer)?;
                        let count_buffer = count_buffer
                            .as_ref()
                            .map(|r| self.get_type(r, IdentifierType::Buffer))
                            .transpose()?;
                        (actual_type, DispatchContent::ExecuteIndirect {
                            signature,
                            pipeline_kind,
                            argument_buffer,
                            count_buffer,
                        })
                    }
                };

                let pipeline = self.get_type(pipeline, pipeline_type)?;
                let root_sig = root_sig
                    .as_ref()
                    .map(|r| self.get_type(r, IdentifierType::RootSig))
                    .transpose()?;
                let root_val_ids = root_val.resolve(self, backend)?;

                self.download_cache.clear();
                let time = backend.dispatch(pipeline, &root_val_ids, content, root_sig, dir)?;
                let name = &self.source_files[identifier.source_file].0;
                let line = self.source_files[identifier.source_file]
                    .2
                    .read_span(&identifier.span.clone().into(), 0, 0)
                    .unwrap()
                    .line()
                    + 1;
                info!(
                    time = %humantime::format_duration(time),
                    "Dispatch from {name}:{line} finished"
                );
            }
            Directive::Include { identifier, path } => {
                // Make path relative to the file it is included from
                let new_path = Path::new(&path.content);
                let cur_path = &self.source_files[identifier.source_file].1;
                let path =
                    cur_path.parent().map(|p| p.join(new_path)).unwrap_or_else(|| new_path.into());

                let dirs = self.parse_file(&path)?;
                self.apply_directives(backend, &dirs)?;
            }
            Directive::Sleep { duration, .. } => std::thread::sleep(duration.content),
            Directive::Dump { resource, typ, format, print_stride, .. } => match typ {
                DumpDataType::DataType(typ) => {
                    let resource_id = self.get_type(resource, IdentifierType::Buffer)?;
                    let data = self.download(backend, resource_id)?;

                    let vals = Values::from_data(*typ, data);
                    let size = vals.data.first().unwrap().len();

                    // If we don't have a stride, take chunk size data.len(),
                    // i.e., we have only one chunk.
                    // Otherwise, the chunk length is size * stride
                    let chunk_size = print_stride
                        .map_or(data.len(), |s| usize::try_from(s.get()).unwrap() * size);

                    let slices = data.chunks(chunk_size);

                    // align values to max_val_len, but only if the output is strided
                    let max_val_len = print_stride.map_or(0, |_| {
                        vals.data.iter().map(|v| v.to_string().len()).max().unwrap_or_default()
                    });
                    let max_off_len = (data.len() - chunk_size).to_string().len();

                    for (index, slice) in slices.enumerate() {
                        let position = index * chunk_size;
                        let sliced_vals = Values::from_data(*typ, slice);
                        match *format {
                            DumpFormat::List => {
                                info!(
                                    "{}[{position:max_off_len$}]: {sliced_vals:max_val_len$}",
                                    resource.content
                                )
                            }
                            DumpFormat::Expect => info!(
                                "EXPECT {} {typ} OFFSET {position:max_off_len$} EQ \
                                 {sliced_vals:max_val_len$}",
                                resource.content
                            ),
                        };
                    }
                }
                DumpDataType::Dxil => {
                    let resource_id = self.get_type(resource, IdentifierType::Object)?;
                    let assembly = backend.disassemble_dxil(resource_id, dir)?;
                    info!("DXIL for {}:\n{assembly}", resource.content);
                }
            },
            Directive::Expect { resource, offset, values, value_spans, epsilon, .. } => {
                let resource_id = self.get_type(resource, IdentifierType::Buffer)?;
                let ignore_expect = self.args.ignore_expect;
                let values = values.resolve(self, backend)?;
                let data = self.download(backend, resource_id)?;
                let offset_usize = usize::try_from(offset.content).context("Too large offset")?;
                let offset = offset.map_content(offset_usize);
                let values_len = values.byte_len();
                let data_len = data.len();

                if offset.content > data_len || offset.content + values_len > data_len {
                    return Err(error::ExpectOutOfBounds {
                        buffer: resource.clone(),
                        buffer_size: data_len,
                        offset: offset.clone(),
                        expect_size: values_len,
                    }
                    .into());
                }

                if ignore_expect {
                    // Don't check
                    return Ok(());
                }

                let data = &data[offset.content..(offset.content + values_len)];
                if let Some(epsilon) = epsilon {
                    let buffer_content = Values::from_data(epsilon.content.get_type(), data);

                    if let Err(i) = values.compare_epsilon(&buffer_content, epsilon.content) {
                        return Err(error::ExpectEpsilon {
                            first: value_spans[i].clone(),
                            buffer: resource.clone(),
                            buffer_first: buffer_content.data[i],
                            epsilon: epsilon.content,
                            buffer_content,
                            expected: values,
                            offset: offset_usize,
                        }
                        .into());
                    }
                } else {
                    let expected = values.get_data();
                    for (byte, (d, e)) in data.iter().zip(expected.iter()).enumerate() {
                        if d != e {
                            let mut prefix = 0;
                            let i = values.data.iter().position(|v| {
                                prefix += v.len();
                                prefix > byte
                            });
                            let i = i.expect("Must be in bounds");
                            let buffer_content = Values::from_data_using_types(data, &values);
                            return Err(error::Expect {
                                first: value_spans[i].clone(),
                                buffer: resource.clone(),
                                buffer_first: buffer_content.data[i],
                                buffer_content,
                                expected: values,
                                offset: offset_usize,
                            }
                            .into());
                        }
                    }
                    debug_assert_eq!(data, expected, "EXPECT failed");
                }
            }
            Directive::AssertShaderId { equal, id_a, id_b, .. } => {
                let idx_a = self.get_type(id_a, IdentifierType::ShaderId)?;
                let idx_b = self.get_type(id_b, IdentifierType::ShaderId)?;
                let content_a = backend.get_shader_id(idx_a)?;
                let content_b = backend.get_shader_id(idx_b)?;

                if self.args.ignore_expect {
                    // Don't check
                    return Ok(());
                }

                if (content_a == content_b) != *equal {
                    return Err(error::AssertShaderId {
                        id_a: id_a.clone(),
                        id_b: id_b.clone(),
                        content_a: Values::from_data(DataType::U8, &content_a),
                        content_b: Values::from_data(DataType::U8, &content_b),
                        equal: *equal,
                    }
                    .into());
                }
            }
        }
        Ok(())
    }
}

// Enable colors on Windows in powershell and default terminal (works without this in the new
// Windows Terminal)
#[cfg(feature = "enable_dx12")]
fn enable_terminal_colors() -> Result<()> {
    unsafe {
        use windows::Win32::System::Console;
        let stdout =
            Console::GetStdHandle(Console::STD_OUTPUT_HANDLE).context("Get stdout handle")?;
        let mut mode = Default::default();
        Console::GetConsoleMode(stdout, &mut mode).context("Get console mode")?;
        mode |= Console::ENABLE_PROCESSED_OUTPUT;
        mode |= Console::ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        Console::SetConsoleMode(stdout, mode).context("Set console mode")?;
    }
    Ok(())
}

fn main() -> Result<()> {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }

    tracing_subscriber::fmt::init();

    // Enable colors on Windows in powershell and default terminal (works without this in the new
    // Windows Terminal)
    #[cfg(feature = "enable_dx12")]
    {
        if let Err(error) = enable_terminal_colors() {
            trace!(%error, "Failed to get stdout handle for setting color support");
        }
    }

    let args = Args::parse();
    debug!("Started");

    if args.list_devices {
        debug!("Listing devices");
        let devices = backend::devices(args.backend).context("Listing devices")?;
        let mut s = String::new();
        for d in devices {
            s.push_str("\n- ");
            s.push_str(&d);
        }
        info!(devices = %s, "Device list");
        return Ok(());
    }

    let mut state = State::new(args.clone());

    let directives = if let Some(path) = &args.filename {
        state.parse_file(path)
    } else {
        let mut stdin_reader = BufReader::new(std::io::stdin());
        state.parse_stream("stdin".into(), Path::new(""), &mut stdin_reader)
    }?;

    // Create backend
    debug!("Creating backend");
    if args.window {
        // Create window
        // Struct shared between render callback and code outside
        #[derive(Debug, Default)]
        struct RenderState {
            error: Option<miette::Report>,
        }

        let render_state = Rc::new(RefCell::new(RenderState::default()));
        let render_state2 = render_state.clone();

        let max_frames = state.args.repeat.unwrap_or_default();
        let mut frame = 0;
        let mut window =
            backend::create_with_window(args.backend, &state.args.clone(), move |backend| {
                // Render callback
                if let Err(error) = state.run(backend, &directives) {
                    debug!(%frame, %error, "Got error when rendering frame");
                    let mut render_state = render_state.borrow_mut();
                    render_state.error = Some(error);
                    return Continue::Exit;
                }
                frame += 1;
                if max_frames == 0 || frame < max_frames {
                    state.reset();
                    backend.reset();
                    Continue::Continue
                } else {
                    Continue::Exit
                }
            })
            .context("Creating the backend")?;
        window.main_loop()?;

        let mut render_state = render_state2.borrow_mut();
        if let Some(error) = render_state.error.take() {
            return Err(error);
        }
    } else {
        let mut backend =
            backend::create(args.backend, &state.args).context("Creating the backend")?;

        for _ in 0..state.args.repeat.unwrap_or(1) {
            state.run(&mut *backend, &directives)?;
            state.reset();
            backend.reset();
        }
    }
    info!("Success");

    Ok(())
}
