// Copyright (c) Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! DirectX 12 implementation to run scripts.
use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt::{Display, Write};
use std::marker::PhantomData;
use std::mem::{self, ManuallyDrop};
use std::ops::{self, Deref, DerefMut};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::{ptr, slice, str};

use half::f16;
use miette::{Report, Result, bail, miette};
use tracing::{debug, error, info, trace, warn};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Graphics::Direct3D;
use windows::Win32::Graphics::Direct3D::Dxc;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi;
use windows::Win32::Graphics::Dxgi::Common as dxgi;
use windows::Win32::System::Diagnostics::Debug;
use windows::Win32::System::Kernel;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;
use windows::core::{HSTRING, Interface, PCSTR, PCWSTR};

use crate::backend::{Backend, Continue, RenderCallback, Window};
use crate::parser::Identifier;
use crate::{
    Aabb, AccelStructConfig, CommandSignatureArgument, DataType, Directive, DispatchContent,
    DispatchType, Export, GeometryConfig, IdentifierIdx, IdentifierType, InputViewType,
    PipelineKind, PipelineStateObjectType, ResultExt, RootSigConfig, RootSigConst, RootSigEntry,
    RootSigTable, RootSigView, RootVal, ShaderReference, StateObjectConfig, TlasBlasConfig,
    Transform, UnresolvedShaderTableRecord, ViewType, error,
};

#[cfg(feature = "enable_cpp")]
#[cxx::bridge]
mod ffi {
    unsafe extern "C++" {
        include!("smoldr/src/backend/Dx12Helpers.h");
        type IDxcCompiler3;
        type IDxcOperationResult;

        unsafe fn compile(
            compiler: *mut IDxcCompiler3, code: &str, args: &[*const u16],
        ) -> Result<*mut IDxcOperationResult>;
    }
}

// Assert that all the config bits are the same as in dx.

const _: () =
    assert!(GeometryConfig::OPAQUE.bits() == D3D12_RAYTRACING_GEOMETRY_FLAG_OPAQUE.0 as u32);
const _: () = assert!(
    GeometryConfig::NO_DUPLICATE_ANYHIT.bits()
        == D3D12_RAYTRACING_GEOMETRY_FLAG_NO_DUPLICATE_ANYHIT_INVOCATION.0 as u32
);

const _: () = assert!(
    AccelStructConfig::ALLOW_UPDATE.bits()
        == D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_ALLOW_UPDATE.0 as u32
);
const _: () = assert!(
    AccelStructConfig::ALLOW_COMPACTION.bits()
        == D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_ALLOW_COMPACTION.0 as u32
);
const _: () = assert!(
    AccelStructConfig::PREFER_FAST_TRACE.bits()
        == D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE.0 as u32
);
const _: () = assert!(
    AccelStructConfig::PREFER_FAST_BUILD.bits()
        == D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_BUILD.0 as u32
);
const _: () = assert!(
    AccelStructConfig::MINIMIZE_MEMORY.bits()
        == D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_MINIMIZE_MEMORY.0 as u32
);

const _: () = assert!(
    RootSigConfig::ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT.bits()
        == D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT.0 as u32
);
const _: () = assert!(
    RootSigConfig::DENY_VERTEX_SHADER_ROOT_ACCESS.bits()
        == D3D12_ROOT_SIGNATURE_FLAG_DENY_VERTEX_SHADER_ROOT_ACCESS.0 as u32
);
const _: () = assert!(
    RootSigConfig::DENY_HULL_SHADER_ROOT_ACCESS.bits()
        == D3D12_ROOT_SIGNATURE_FLAG_DENY_HULL_SHADER_ROOT_ACCESS.0 as u32
);
const _: () = assert!(
    RootSigConfig::DENY_DOMAIN_SHADER_ROOT_ACCESS.bits()
        == D3D12_ROOT_SIGNATURE_FLAG_DENY_DOMAIN_SHADER_ROOT_ACCESS.0 as u32
);
const _: () = assert!(
    RootSigConfig::DENY_GEOMETRY_SHADER_ROOT_ACCESS.bits()
        == D3D12_ROOT_SIGNATURE_FLAG_DENY_GEOMETRY_SHADER_ROOT_ACCESS.0 as u32
);
const _: () = assert!(
    RootSigConfig::DENY_PIXEL_SHADER_ROOT_ACCESS.bits()
        == D3D12_ROOT_SIGNATURE_FLAG_DENY_PIXEL_SHADER_ROOT_ACCESS.0 as u32
);
const _: () = assert!(
    RootSigConfig::ALLOW_STREAM_OUTPUT.bits()
        == D3D12_ROOT_SIGNATURE_FLAG_ALLOW_STREAM_OUTPUT.0 as u32
);
const _: () = assert!(
    RootSigConfig::LOCAL_ROOT_SIGNATURE.bits()
        == D3D12_ROOT_SIGNATURE_FLAG_LOCAL_ROOT_SIGNATURE.0 as u32
);
const _: () = assert!(
    RootSigConfig::DENY_AMPLIFICATION_SHADER_ROOT_ACCESS.bits()
        == D3D12_ROOT_SIGNATURE_FLAG_DENY_AMPLIFICATION_SHADER_ROOT_ACCESS.0 as u32
);
const _: () = assert!(
    RootSigConfig::DENY_MESH_SHADER_ROOT_ACCESS.bits()
        == D3D12_ROOT_SIGNATURE_FLAG_DENY_MESH_SHADER_ROOT_ACCESS.0 as u32
);
const _: () = assert!(
    RootSigConfig::CBV_SRV_UAV_HEAP_DIRECTLY_INDEXED.bits()
        == D3D12_ROOT_SIGNATURE_FLAG_CBV_SRV_UAV_HEAP_DIRECTLY_INDEXED.0 as u32
);
const _: () = assert!(
    RootSigConfig::SAMPLER_HEAP_DIRECTLY_INDEXED.bits()
        == D3D12_ROOT_SIGNATURE_FLAG_SAMPLER_HEAP_DIRECTLY_INDEXED.0 as u32
);

const _: () = assert!(
    TlasBlasConfig::TRIANGLE_CULL_DISABLE.bits()
        == D3D12_RAYTRACING_INSTANCE_FLAG_TRIANGLE_CULL_DISABLE.0 as u32
);
const _: () = assert!(
    TlasBlasConfig::TRIANGLE_FRONT_COUNTERCLOCKWISE.bits()
        == D3D12_RAYTRACING_INSTANCE_FLAG_TRIANGLE_FRONT_COUNTERCLOCKWISE.0 as u32
);
const _: () = assert!(
    TlasBlasConfig::FORCE_OPAQUE.bits() == D3D12_RAYTRACING_INSTANCE_FLAG_FORCE_OPAQUE.0 as u32
);
const _: () = assert!(
    TlasBlasConfig::FORCE_NON_OPAQUE.bits()
        == D3D12_RAYTRACING_INSTANCE_FLAG_FORCE_NON_OPAQUE.0 as u32
);

const _: () = assert!(
    StateObjectConfig::LOCAL_DEP_ON_EXTERNAL.bits()
        == D3D12_STATE_OBJECT_FLAG_ALLOW_LOCAL_DEPENDENCIES_ON_EXTERNAL_DEFINITIONS.0 as u32
);
const _: () = assert!(
    StateObjectConfig::EXTERNAL_DEP_ON_LOCAL.bits()
        == D3D12_STATE_OBJECT_FLAG_ALLOW_EXTERNAL_DEPENDENCIES_ON_LOCAL_DEFINITIONS.0 as u32
);
const _: () = assert!(
    StateObjectConfig::ADD_TO_SO.bits()
        == D3D12_STATE_OBJECT_FLAG_ALLOW_STATE_OBJECT_ADDITIONS.0 as u32
);

// End assertion block

type ShaderId = [u8; D3D12_SHADER_IDENTIFIER_SIZE_IN_BYTES as usize];

/// Number of buffers in the swap chain.
const RENDER_TARGETS: u32 = 2;

struct Dx12Cleanup;

pub(crate) struct Dx12Backend {
    debug_controller: Option<ID3D12Debug1>,
    device: ID3D12Device7,
    command_queue: ID3D12CommandQueue,
    fence: ID3D12Fence1,
    fence_value: AtomicU64,
    command_allocator: ID3D12CommandAllocator,
    command_list: ID3D12GraphicsCommandList4,
    descriptor_heap: Option<ID3D12DescriptorHeap>,
    /// Amount of used bytes on the descriptor heap
    descriptor_heap_size: u64,
    query_heap: Option<ID3D12QueryHeap>,
    query_buffer: Option<ID3D12Resource2>,
    /// Caches if raytracing is supported.
    supports_raytracing: Option<bool>,

    /// Compiled HLSL sources as DXIL containers
    objects: HashMap<IdentifierIdx, Dxc::IDxcBlob>,
    buffers: HashMap<IdentifierIdx, ID3D12Resource2>,
    root_sigs: HashMap<IdentifierIdx, ID3D12RootSignature>,
    shader_ids: HashMap<IdentifierIdx, ShaderId>,
    /// Buffer that stores the shader table and its stride
    shader_tables: HashMap<IdentifierIdx, (ID3D12Resource2, usize)>,
    /// (pipeline, root signature)
    pipelines: HashMap<IdentifierIdx, (ID3D12PipelineState, Option<IdentifierIdx>)>,
    psos: HashMap<IdentifierIdx, ID3D12StateObject>,
    views: HashMap<IdentifierIdx, DescriptorHandles>,
    blas: HashMap<IdentifierIdx, ID3D12Resource2>,
    tlas: HashMap<IdentifierIdx, ID3D12Resource2>,
    /// (command signature, stride)
    command_signatures: HashMap<IdentifierIdx, (ID3D12CommandSignature, u32)>,
    _cleanup: Dx12Cleanup,
}

struct Dx12Window {
    // swap_chain first, so it is dropped first
    swap_chain: Dxgi::IDXGISwapChain3,
    back_buffer_views: [D3D12_CPU_DESCRIPTOR_HANDLE; RENDER_TARGETS as usize],
    /// Descriptor heap for render targets
    render_target_heap: ID3D12DescriptorHeap,

    window: HWND,
    backend: Dx12Backend,
    render_callback: RenderCallback,
}

#[derive(Debug)]
struct DescriptorHandles {
    cpu: D3D12_CPU_DESCRIPTOR_HANDLE,
    gpu: D3D12_GPU_DESCRIPTOR_HANDLE,
}

struct CommandList {
    command_list: ID3D12GraphicsCommandList4,
}

struct MappedBuffer<Mode: AccessMode> {
    buffer: ID3D12Resource2,
    len: usize,
    addr: *mut c_void,
    /// The struct depends on the AccessMode, so we need to use it, but we do not need to store it.
    /// Add a phantom use for it.
    _phantom: std::marker::PhantomData<Mode>,
}

/// Wrapper for miette::Context that also gets the inner error for device removed errors.
trait HandleErr<T, E> {
    fn handle_err<D: Display + Send + Sync + 'static>(
        self, device: &ID3D12Device7, msg: D,
    ) -> Result<T, Report>;
    #[allow(dead_code)]
    fn with_handle_err<D: Display + Send + Sync + 'static, F: FnOnce() -> D>(
        self, device: &ID3D12Device7, f: F,
    ) -> Result<T, Report>;

    fn h_err<D: Display + Send + Sync + 'static>(
        self, backend: &Dx12Backend, msg: D,
    ) -> Result<T, Report>;
    fn with_h_err<D: Display + Send + Sync + 'static, F: FnOnce() -> D>(
        self, backend: &Dx12Backend, f: F,
    ) -> Result<T, Report>;
}

macro_rules! resource_barrier {
    ($cmds:ident($resource:expr, $stateBefore:expr, $stateAfter:expr $(,)?)) => {{
        let barrier = D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: std::mem::transmute_copy($resource),
                    Subresource: 0,
                    StateBefore: $stateBefore,
                    StateAfter: $stateAfter,
                }),
            },
            ..Default::default()
        };

        $cmds.ResourceBarrier(&[barrier]);
    }};
}

const EMPTY_RANGE: D3D12_RANGE = D3D12_RANGE { Begin: 0, End: 0 };

trait IsTrue {}
struct True {}
impl IsTrue for True {}

trait AccessMode {
    type CanRead;
    type CanWrite;

    fn read_range() -> Option<*const D3D12_RANGE> { None }
    fn write_range() -> Option<*const D3D12_RANGE> { None }
}

struct ReadOnlyAccess {}

impl AccessMode for ReadOnlyAccess {
    type CanRead = True;
    type CanWrite = ();

    fn write_range() -> Option<*const D3D12_RANGE> { Some(&EMPTY_RANGE) }
}

struct WriteOnlyAccess {}

impl AccessMode for WriteOnlyAccess {
    type CanRead = ();
    type CanWrite = True;

    fn read_range() -> Option<*const D3D12_RANGE> { Some(&EMPTY_RANGE) }
}

// Never constructed
#[allow(dead_code)]
struct ReadWriteAccess {}

impl AccessMode for ReadWriteAccess {
    type CanRead = True;
    type CanWrite = True;
}

impl<Mode: AccessMode> MappedBuffer<Mode> {
    fn new(buffer: ID3D12Resource2) -> Result<Self> {
        let mut addr = ptr::null_mut();
        let len;
        unsafe {
            let buffer_desc = buffer.GetDesc1();
            len = buffer_desc.Width * u64::from(buffer_desc.Height);

            let mut device = None;
            let _ = buffer.GetDevice(&mut device);
            buffer
                .Map(0, Mode::read_range(), Some(&mut addr))
                .handle_err(&device.unwrap(), "Mapping buffer to CPU memory")?;

            Ok(Self {
                buffer,
                len: len.try_into().context("Too large buffer")?,
                addr,
                _phantom: Default::default(),
            })
        }
    }
}

impl<Mode: AccessMode> Deref for MappedBuffer<Mode> {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        unsafe { slice::from_raw_parts(self.addr.cast(), self.len) }
    }
}

impl<Mode: AccessMode> DerefMut for MappedBuffer<Mode>
where Mode::CanWrite: IsTrue
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { slice::from_raw_parts_mut(self.addr.cast(), self.len) }
    }
}

impl<Mode: AccessMode> Drop for MappedBuffer<Mode> {
    fn drop(&mut self) {
        unsafe {
            self.buffer.Unmap(0, Mode::write_range());
        }
    }
}

trait NextMultipleTrait:
    ops::Add<Self, Output = Self>
    + ops::Rem<Self, Output = Self>
    + ops::Sub<Self, Output = Self>
    + Copy
    + Eq
    + Sized
{
    fn zero() -> Self;
}

impl NextMultipleTrait for usize {
    fn zero() -> Self { 0 }
}

impl NextMultipleTrait for u64 {
    fn zero() -> Self { 0 }
}

fn next_multiple_of<T: NextMultipleTrait>(val: T, align: T) -> T {
    let r = val % align;
    if r == T::zero() { val } else { val + (align - r) }
}

fn err_device_removed<T>(
    r: Result<T, windows::core::Error>, device: &ID3D12Device7,
) -> Result<T, windows::core::Error> {
    match r {
        Ok(r) => Ok(r),
        Err(e) => {
            if e.code() == Dxgi::DXGI_ERROR_DEVICE_REMOVED {
                let reason = unsafe { device.GetDeviceRemovedReason() };
                Err(reason.err().unwrap_or(e))
            } else {
                Err(e)
            }
        }
    }
}

impl<T> HandleErr<T, windows::core::Error> for Result<T, windows::core::Error>
where Result<T, windows::core::Error>: crate::ResultExt<T>
{
    fn handle_err<D: Display + Send + Sync + 'static>(
        self, device: &ID3D12Device7, msg: D,
    ) -> Result<T, Report> {
        crate::ResultExt::context(err_device_removed(self, device), msg)
    }

    fn with_handle_err<D: Display + Send + Sync + 'static, F: FnOnce() -> D>(
        self, device: &ID3D12Device7, f: F,
    ) -> Result<T, Report> {
        crate::ResultExt::with_context(err_device_removed(self, device), f)
    }

    fn h_err<D: Display + Send + Sync + 'static>(
        self, backend: &Dx12Backend, msg: D,
    ) -> Result<T, Report> {
        crate::ResultExt::context(err_device_removed(self, &backend.device), msg)
    }

    fn with_h_err<D: Display + Send + Sync + 'static, F: FnOnce() -> D>(
        self, backend: &Dx12Backend, f: F,
    ) -> Result<T, Report> {
        crate::ResultExt::with_context(err_device_removed(self, &backend.device), f)
    }
}

impl CommandList {
    fn new(
        command_list: ID3D12GraphicsCommandList4, command_allocator: &ID3D12CommandAllocator,
        name: &str, pipeline: Option<&ID3D12PipelineState>,
    ) -> Result<Self> {
        unsafe {
            let mut device = None;
            let _ = command_list.GetDevice(&mut device);
            command_list
                .Reset(command_allocator, pipeline)
                .handle_err(device.as_ref().unwrap(), "Resetting command list")?;

            command_list
                .SetName(&HSTRING::from(name))
                .handle_err(&device.unwrap(), "Set command list name")?;
        }
        Ok(Self { command_list })
    }

    fn run(self, backend: &Dx12Backend) -> Result<()> {
        unsafe {
            self.command_list.Close().h_err(backend, "Closing command list")?;

            trace!("Execute command list");
            backend.command_queue.ExecuteCommandLists(&[Some(self.command_list.into())]);
            backend.fence()?;
            trace!("Command list finished");
        }
        Ok(())
    }
}

impl Deref for CommandList {
    type Target = ID3D12GraphicsCommandList4;
    fn deref(&self) -> &Self::Target { &self.command_list }
}

impl Dx12Backend {
    /// Get dxil container out of a compilation result and return an error if the result is not a
    /// success.
    fn create_object_from_result(compile_res: Dxc::IDxcOperationResult) -> Result<Dxc::IDxcBlob> {
        unsafe {
            let status = compile_res.GetStatus().context("Getting HLSL compilation status")?;
            let error = compile_res.GetErrorBuffer().context("Getting HLSL compilation errors")?;
            let error_str = error
                .cast::<Dxc::IDxcBlobUtf8>()
                .context("Getting HLSL compilation errors as UTF-8")?
                .GetStringPointer()
                .to_string()
                .context("Converting HLSL compilation errors to a string")?;
            status.ok().with_context(|| error_str.clone())?;

            if !error_str.is_empty() {
                warn!(error = %error_str, "Warnings while compiling HLSL");
            }

            compile_res.GetResult().context("Getting HLSL compilation result")
        }
    }

    /// Creates a descriptor heap if there is not already one
    fn descriptor_handles(&mut self) -> Result<DescriptorHandles> {
        unsafe {
            let heap = if let Some(h) = &self.descriptor_heap {
                h
            } else {
                let desc = D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                    NumDescriptors: 256,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    ..Default::default()
                };
                let heap = self
                    .device
                    .CreateDescriptorHeap(&desc)
                    .h_err(self, "Creating descriptor heap")?;
                self.descriptor_heap = Some(heap);
                self.descriptor_heap.as_ref().unwrap()
            };
            let offset = self.descriptor_heap_size;
            self.descriptor_heap_size += u64::from(
                self.device
                    .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV),
            );
            let mut cpu = heap.GetCPUDescriptorHandleForHeapStart();
            let mut gpu = heap.GetGPUDescriptorHandleForHeapStart();
            cpu.ptr += offset as usize;
            gpu.ptr += offset;
            Ok(DescriptorHandles { cpu, gpu })
        }
    }

    fn get_query_heap(&mut self) -> Result<&ID3D12QueryHeap> {
        if self.query_heap.is_none() {
            unsafe {
                let query_count = 2;

                let desc = D3D12_QUERY_HEAP_DESC {
                    Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
                    Count: query_count,
                    ..Default::default()
                };
                // WIN-FIXME Should return heap like CreateDescriptorHeap
                self.device
                    .CreateQueryHeap(&desc, &mut self.query_heap)
                    .h_err(self, "Creating query heap")?;
                let h = self.query_heap.as_ref().unwrap();
                h.SetName(w!("query heap")).h_err(self, "Set query heap name")?;

                let buffer_desc = D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Width: u64::from(query_count) * mem::size_of::<u64>() as u64,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    SampleDesc: dxgi::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    ..Default::default()
                };
                let heap_props =
                    D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_READBACK, ..Default::default() };

                let mut buffer: Option<ID3D12Resource2> = None;
                self.device
                    .CreateCommittedResource(
                        &heap_props,
                        D3D12_HEAP_FLAG_NONE,
                        &buffer_desc,
                        D3D12_RESOURCE_STATE_COPY_DEST,
                        None,
                        &mut buffer,
                    )
                    .h_err(self, "Creating query buffer")?;
                buffer
                    .as_ref()
                    .unwrap()
                    .SetName(w!("query buffer"))
                    .h_err(self, "Set query buffer name")?;
                self.query_buffer = buffer;
            }
        }
        Ok(self.query_heap.as_ref().unwrap())
    }

    fn create_buffer_intern(
        &self, size: u64, alignment: u64, heap: D3D12_HEAP_TYPE, flags: D3D12_RESOURCE_FLAGS,
        state: D3D12_RESOURCE_STATES, name: &str,
    ) -> Result<ID3D12Resource2> {
        trace!(%size, %name, "Create buffer");

        let buffer_desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: alignment.max(D3D12_DEFAULT_RESOURCE_PLACEMENT_ALIGNMENT.into()),
            Width: size,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: dxgi::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: flags,
            ..Default::default()
        };
        let heap_props = D3D12_HEAP_PROPERTIES { Type: heap, ..Default::default() };

        unsafe {
            let mut buffer = None;
            self.device
                .CreateCommittedResource(
                    &heap_props,
                    D3D12_HEAP_FLAG_NONE,
                    &buffer_desc,
                    state,
                    None,
                    &mut buffer,
                )
                .with_h_err(self, || format!("Creating buffer '{name}'"))?;

            let buffer: ID3D12Resource2 =
                buffer.ok_or_else(|| miette!("Failed to create buffer '{name}'"))?;
            buffer
                .SetName(&HSTRING::from(name))
                .with_h_err(self, || format!("Set buffer name '{name}'"))?;

            Ok(buffer)
        }
    }

    fn upload_intern(&self, buffer: &ID3D12Resource2, f: &mut dyn FnMut(&mut [u8])) -> Result<()> {
        unsafe {
            let buffer_desc = buffer.GetDesc1();
            let size = buffer_desc.Width * u64::from(buffer_desc.Height);

            let upload_buffer = self.create_buffer_intern(
                size,
                0,
                D3D12_HEAP_TYPE_UPLOAD,
                D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE,
                D3D12_RESOURCE_STATE_GENERIC_READ,
                "Upload buffer",
            )?;
            {
                let mut upload = MappedBuffer::<WriteOnlyAccess>::new(upload_buffer.clone())?;
                f(&mut upload);
            }

            let cmds = self.command_list("upload")?;

            resource_barrier!(cmds(
                buffer,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_DEST,
            ));

            cmds.CopyResource(buffer, &upload_buffer);
            cmds.run(self)?;
        }
        Ok(())
    }

    fn build_accel_struct_intern(
        &self, name: &str, inputs: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS,
    ) -> Result<ID3D12Resource2> {
        unsafe {
            let mut build_info = Default::default();
            // WIN-FIXME Should return result
            self.device.GetRaytracingAccelerationStructurePrebuildInfo(&inputs, &mut build_info);

            let buffer = self.create_buffer_intern(
                build_info.ResultDataMaxSizeInBytes,
                u64::from(D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BYTE_ALIGNMENT),
                D3D12_HEAP_TYPE_DEFAULT,
                D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
                &format!("AS {name}"),
            )?;

            let scratch_buffer = self.create_buffer_intern(
                build_info.ScratchDataSizeInBytes,
                u64::from(D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BYTE_ALIGNMENT),
                D3D12_HEAP_TYPE_DEFAULT,
                D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COMMON,
                &format!("AS {name} scratch"),
            )?;

            let desc = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
                DestAccelerationStructureData: buffer.GetGPUVirtualAddress(),
                Inputs: inputs,
                ScratchAccelerationStructureData: scratch_buffer.GetGPUVirtualAddress(),
                ..Default::default()
            };

            let cmds = self.command_list(&format!("build as {name}"))?;
            cmds.BuildRaytracingAccelerationStructure(&desc, None);
            cmds.run(self)?;

            Ok(buffer)
        }
    }

    fn get_pso_shader_id(&self, pso: IdentifierIdx, name: &Identifier) -> Result<ShaderId> {
        let props: ID3D12StateObjectProperties = self.psos[&pso]
            .cast()
            .h_err(self, "Casting pipeline state object to state object properties")?;
        let s = HSTRING::from(&name.content);
        unsafe {
            let p = props.GetShaderIdentifier(&s);
            if p.is_null() {
                return Err(error::NullShaderId { name: name.clone() }.into());
            }
            let s = slice::from_raw_parts(p.cast(), D3D12_SHADER_IDENTIFIER_SIZE_IN_BYTES as usize);
            Ok(s.try_into().unwrap())
        }
    }

    // Find the maximum length of the local root signature
    fn get_local_root_sig_stride(&self, records: &[UnresolvedShaderTableRecord]) -> usize {
        let el_size = mem::size_of::<u64>();
        let len = records
            .iter()
            .flat_map(|r| {
                r.root_val
                    .binds
                    .iter()
                    .map(|b| b.index as usize * el_size)
                    .chain(r.root_val.consts.iter().map(|c| {
                        c.index as usize * el_size + next_multiple_of(c.content.len(), el_size)
                    }))
                    .chain(r.root_val.views.iter().map(|v| v.index as usize * el_size))
            })
            .max()
            .map(|c| c + 1)
            .unwrap_or_default();

        next_multiple_of(
            len + D3D12_SHADER_IDENTIFIER_SIZE_IN_BYTES as usize,
            D3D12_RAYTRACING_SHADER_RECORD_BYTE_ALIGNMENT as usize,
        )
    }

    fn supports_raytracing(&mut self) -> Result<()> {
        if self.supports_raytracing.is_none() {
            let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS5::default();
            unsafe {
                self.device
                    .CheckFeatureSupport(
                        D3D12_FEATURE_D3D12_OPTIONS5,
                        (&mut options as *mut D3D12_FEATURE_DATA_D3D12_OPTIONS5).cast(),
                        mem::size_of_val(&options) as u32,
                    )
                    .h_err(self, "Check feature support")?;
            }
            debug!(?options, "D3D12 features");
            self.supports_raytracing =
                Some(options.RaytracingTier != D3D12_RAYTRACING_TIER_NOT_SUPPORTED);
        }

        if let Some(true) = self.supports_raytracing {
            Ok(())
        } else {
            miette::bail!("Trying to use raytracing, but it is not supported on this device");
        }
    }

    fn command_list(&self, name: &str) -> Result<CommandList> {
        CommandList::new(self.command_list.clone(), &self.command_allocator, name, None)
    }

    fn command_list_with_pipeline(
        &self, name: &str, pipeline: &ID3D12PipelineState,
    ) -> Result<CommandList> {
        CommandList::new(self.command_list.clone(), &self.command_allocator, name, Some(pipeline))
    }

    fn fence(&self) -> Result<()> {
        let fence_value = self.fence_value.fetch_add(1, Ordering::Relaxed) + 1;
        unsafe {
            self.command_queue
                .Signal(&self.fence, fence_value)
                .h_err(self, "Signal fence in command queue")?;

            let event_handle = Threading::CreateEventA(None, false, false, None)
                .h_err(self, "Create event for fence")?;
            self.fence
                .SetEventOnCompletion(fence_value, event_handle)
                .h_err(self, "Trigger event on fence")?;
            trace!("Waiting for command list to finish");
            let wait_res = Threading::WaitForSingleObject(event_handle, Threading::INFINITE);
            if wait_res != WAIT_OBJECT_0 {
                bail!("Failed to wait for command list (wait result {:?})", wait_res);
            }
        }
        Ok(())
    }

    fn get_adapter_desc(adapter: &Dxgi::IDXGIAdapter) -> Result<String> {
        let desc = unsafe { adapter.GetDesc() }.context("Getting dxgi adapter description")?;
        let mut desc_str =
            String::from_utf16(&desc.Description).context("Parse dxgi adapter description")?;

        // Take until 0-byte
        if let Some(i) = desc_str.find('\0') {
            desc_str.truncate(i);
        }
        Ok(desc_str)
    }

    fn messages(device: &ID3D12Device7) -> Result<String> {
        let mut result = String::new();
        let debug_layer = device
            .cast::<ID3D12InfoQueue>()
            .handle_err(device, "Obtain InfoQueue for debug messages")?;
        unsafe {
            let count = debug_layer.GetNumStoredMessages();

            for i in 0..count {
                let mut len = 0;
                debug_layer
                    .GetMessage(i, None, &mut len)
                    .handle_err(device, "Getting debug message length")?;
                let mut buffer = vec![0u8; len];
                debug_layer
                    .GetMessage(i, Some(buffer.as_mut_ptr().cast()), &mut len)
                    .handle_err(device, "Getting debug message")?;
                let msg: &D3D12_MESSAGE = &*buffer.as_ptr().cast();
                let description =
                    slice::from_raw_parts(msg.pDescription, msg.DescriptionByteLength);
                let description =
                    str::from_utf8(description).context("Parse debug message as string")?;
                writeln!(&mut result, "DX {description}").unwrap();
            }
            debug_layer.ClearStoredMessages();

            // Remove trailing newline
            if let Some(removed) = result.pop() {
                assert_eq!(removed, '\n', "Message is expected to end with a newline");
            }
        }
        Ok(result)
    }
}

unsafe extern "system" fn debug_callback(
    category: D3D12_MESSAGE_CATEGORY, severity: D3D12_MESSAGE_SEVERITY, id: D3D12_MESSAGE_ID,
    description: PCSTR, _context: *mut c_void,
) {
    info!("DX {category:?} {severity:?} ({id:?}): {}", description.display());
}

/// Windows event handler function
unsafe extern "system" fn window_event_handler(
    window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM,
) -> LRESULT {
    trace!(%message, "Got windows message");

    unsafe {
        // Get the window stored in user data
        let user_data = GetWindowLongPtrW(window, GWLP_USERDATA);
        if user_data == 0 {
            // Ignore if no window found
            return DefWindowProcW(window, message, wparam, lparam);
        }
        let win = user_data as *mut Dx12Window;
        let win = &mut *win;

        match message {
            WM_PAINT => {
                if let Err(error) = win.paint() {
                    error!(%error, "Failed to paint window");
                }

                // Do not ValidateRect, so Windows keeps creating WM_PAINT events
                return LRESULT(0);
            }
            WM_SIZE => {
                if wparam.0 as u32 != SIZE_MINIMIZED {
                    if let Err(error) = win.resize() {
                        error!(%error, "Failed to resize window");
                    }
                }

                return LRESULT(0);
            }
            WM_KEYDOWN => {
                use windows::Win32::UI::Input::KeyboardAndMouse::*;
                let vk = VIRTUAL_KEY(wparam.0 as u16);
                if vk == VK_ESCAPE {
                    // Exit on escape
                    debug!("Escape pressed, exiting");
                    PostQuitMessage(0);
                    return LRESULT(0);
                }
            }
            WM_DESTROY => {
                debug!("Destroying window");
                PostQuitMessage(0);
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(window, message, wparam, lparam)
    }
}

// Variables for using the dx12 agility sdk, see readme for more info
#[cfg(feature = "agility_sdk")]
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static D3D12SDKVersion: u32 = match u32::from_str_radix(
    env!(
        "D3D12SDK_VERSION",
        "environment variable `D3D12SDK_VERSION` needs to be set to the version of the used \
         Agility SDK"
    ),
    10,
) {
    Ok(v) => v,
    Err(_) => panic!("environment variable `D3D12SDK_VERSION` must be set to a valid number"),
};

#[cfg(feature = "agility_sdk")]
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static D3D12SDKPath: &[u8; 9] = &b".\\D3D12\\\0"; // Put D3D12Core.dll into a D3D12/ subfolder near smoldr.exe

/// Device used for getting debug messages when an exception is thrown.
static DEVICE: Mutex<Option<ID3D12Device7>> = Mutex::new(None);

unsafe extern "system" fn exception_handler(_: *mut Debug::EXCEPTION_POINTERS) -> i32 {
    let lock = DEVICE.lock().unwrap();
    if let Some(device) = &*lock {
        match Dx12Backend::messages(device) {
            Ok(msg) => {
                if !msg.is_empty() {
                    warn!(messages = %msg);
                }
            }
            Err(error) => warn!(%error, "Failed to get debug messages when handling exception"),
        }
    }
    Kernel::ExceptionContinueExecution.0
}

/// Print objects that still have references somewhere.
#[allow(dead_code)]
fn print_live_objects() {
    println!("Printing live objects");
    unsafe {
        let dxgi_debug: Dxgi::IDXGIDebug =
            Dxgi::DXGIGetDebugInterface1(0).expect("Create DXGI debug interface");
        dxgi_debug
            .ReportLiveObjects(Dxgi::DXGI_DEBUG_ALL, Dxgi::DXGI_DEBUG_RLO_SUMMARY)
            .expect("Report live objects");
    }
}

impl Dx12Window {
    /// Create views for the swap chain buffers. Store on the `render_target_heap`.
    fn create_back_buffer_views(&mut self) -> Result<()> {
        unsafe {
            let cpu = self.render_target_heap.GetCPUDescriptorHandleForHeapStart();
            let increment_size = self
                .backend
                .device
                .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV);

            for i in 0..RENDER_TARGETS {
                let buffer: ID3D12Resource2 =
                    self.swap_chain.GetBuffer(i).h_err(&self.backend, "Get swap chain buffer")?;
                buffer
                    .SetName(&HSTRING::from(&format!("Swap chain buffer {i}")))
                    .h_err(&self.backend, "Set swap chain buffer name")?;
                let mut view = cpu;
                view.ptr += (i as usize) * (increment_size as usize);
                self.backend.device.CreateRenderTargetView(&buffer, None, view);
                self.back_buffer_views[i as usize] = view;
            }
        }
        Ok(())
    }

    /// Run one frame
    fn paint(&mut self) -> Result<()> {
        debug!("Got window paint message");
        unsafe {
            {
                // Clear backbuffer
                let index = self.swap_chain.GetCurrentBackBufferIndex();
                let view = self.back_buffer_views[index as usize];
                let buffer: ID3D12Resource2 = self
                    .swap_chain
                    .GetBuffer(index)
                    .h_err(&self.backend, "Get swap chain buffer")?;
                let cmds = self.backend.command_list(&format!("Clear swap chain buffer"))?;
                resource_barrier!(cmds(
                    &buffer,
                    D3D12_RESOURCE_STATE_PRESENT,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                ));
                cmds.OMSetRenderTargets(1, Some(&view), false, None);
                cmds.ClearRenderTargetView(view, &[0.0; 4], None);
                resource_barrier!(cmds(
                    &buffer,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    D3D12_RESOURCE_STATE_PRESENT,
                ));
                cmds.run(&self.backend)?;
            }

            // Call render callback to draw a frame
            if (self.render_callback)(&mut self.backend) == Continue::Exit {
                DestroyWindow(self.window).h_err(&self.backend, "Destroying window")?;
                return Ok(());
            }
            // Present the swapchain
            let error = self.swap_chain.Present(0, Dxgi::DXGI_PRESENT_ALLOW_TEARING);
            if error.is_err() {
                error!(%error, "Presenting swapchain failed");
            }
            self.backend.fence()?;
        }
        Ok(())
    }

    fn resize(&mut self) -> Result<()> {
        unsafe {
            // Window resized, update swap chain
            let mut rect = Default::default();
            GetClientRect(self.window, &mut rect).h_err(&self.backend, "Get window size")?;
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            debug!(width, height, "Resize swap chain");

            if width != 0 && height != 0 {
                // Passing 0 means use draw area size and leave the rest as is
                self.swap_chain
                    .ResizeBuffers(
                        0,
                        0,
                        0,
                        Dxgi::Common::DXGI_FORMAT_UNKNOWN,
                        Dxgi::DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING,
                    )
                    .h_err(&self.backend, "Resize swap chain buffers")?;
                self.create_back_buffer_views()?;
            }
        }
        Ok(())
    }
}

/// Helper struct that stores a reference in the window user data and removes it again when dropped.
struct WindowToUserData<'a> {
    window: HWND,
    _phantom: PhantomData<&'a mut ()>,
}

impl<'a> WindowToUserData<'a> {
    fn new(window: HWND, data: &'a mut Dx12Window) -> Self {
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, data as *mut Dx12Window as isize);
        }
        Self { window, _phantom: PhantomData }
    }
}

impl Drop for WindowToUserData<'_> {
    fn drop(&mut self) {
        // Reset user data of window
        unsafe {
            SetWindowLongPtrW(self.window, GWLP_USERDATA, 0);
        }
    }
}

impl Window for Dx12Window {
    fn main_loop(&mut self) -> Result<()> {
        // Move self into window user data, so that it can be accessed from `window_event_handler`
        let _user_data = WindowToUserData::new(self.window, self);
        loop {
            unsafe {
                let mut msg = Default::default();
                if GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if msg.message == WM_QUIT {
                    break Ok(());
                }
            }
        }
    }
}

impl Backend for Dx12Backend {
    fn new(args: &crate::Args) -> Result<Self> {
        unsafe {
            let mut debug_controller = None;
            if args.validate {
                let mut controller = None;
                // WIN-FIXME Should return debug controller
                D3D12GetDebugInterface(&mut controller).context("Creating debug controller")?;
                let controller: ID3D12Debug1 =
                    controller.ok_or_else(|| miette!("Failed to create debug controller"))?;
                controller.EnableDebugLayer();
                if !args.no_gpu_validate {
                    controller.SetEnableGPUBasedValidation(true);
                }
                debug_controller = Some(controller);
            }

            // Enable experimental features to allow unsigned dxil.
            // Needed for using self-compiled dxc.
            #[cfg(feature = "agility_sdk")]
            D3D12EnableExperimentalFeatures(1, &D3D12ExperimentalShaderModels, None, None)
                .context("Enabling experimental shader models")?;

            let factory: Dxgi::IDXGIFactory =
                Dxgi::CreateDXGIFactory().context("Creating dxgi factory")?;
            let adapter = factory.EnumAdapters(args.device).context("Getting dxgi adapter")?;

            let desc = Self::get_adapter_desc(&adapter)?;
            info!(name = desc, "Using device");

            let mut device = None;
            D3D12CreateDevice(&adapter, Direct3D::D3D_FEATURE_LEVEL_11_0, &mut device)
                .context("Creating device")?;
            let device: ID3D12Device7 = device.ok_or_else(|| miette!("Failed to create device"))?;

            if debug_controller.is_some() {
                // Log debug layer data
                if let Ok(debug_layer) = device.cast::<ID3D12InfoQueue1>() {
                    // WIN-FIXME Should return callback_id
                    let mut callback_id = 0;
                    debug_layer
                        .RegisterMessageCallback(
                            Some(debug_callback),
                            D3D12_MESSAGE_CALLBACK_FLAG_NONE,
                            ptr::null_mut(),
                            &mut callback_id,
                        )
                        .handle_err(&device, "Register debug message callback")?;
                    if callback_id == 0 {
                        error!("Failed to register debug message callback");
                    } else {
                        debug!("Set debug message callback");
                    }
                } else {
                    // The debug layer throws an exception on some errors.
                    // This results in the program exiting without printing the error message.
                    // Add a handler that prints messages and then continues with normal handling.
                    if Debug::AddVectoredExceptionHandler(1, Some(exception_handler)).is_null() {
                        error!("Failed to register exception handler");
                    }
                }

                *DEVICE.lock().unwrap() = Some(device.clone());
            }

            let desc = D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                ..Default::default()
            };
            let command_queue: ID3D12CommandQueue =
                device.CreateCommandQueue(&desc).handle_err(&device, "Creating command queue")?;
            command_queue
                .SetName(w!("Main command queue"))
                .handle_err(&device, "Set command queue name")?;

            let fence = device
                .CreateFence(0, D3D12_FENCE_FLAG_SHARED)
                .handle_err(&device, "Creating fence")?;

            let command_allocator = device
                .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                .handle_err(&device, "Creating command allocator")?;

            // CreateCommandList1 creates an already closed list
            let command_list: ID3D12GraphicsCommandList4 = device
                .CreateCommandList1(0, D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_LIST_FLAG_NONE)
                .handle_err(&device, "Creating command list")?;

            Ok(Self {
                debug_controller,
                device,
                command_queue,
                fence,
                fence_value: Default::default(),
                command_allocator,
                command_list,
                descriptor_heap: None,
                descriptor_heap_size: 0,
                query_heap: None,
                query_buffer: None,
                supports_raytracing: None,

                objects: Default::default(),
                buffers: Default::default(),
                root_sigs: Default::default(),
                shader_ids: Default::default(),
                shader_tables: Default::default(),
                pipelines: Default::default(),
                psos: Default::default(),
                views: Default::default(),
                blas: Default::default(),
                tlas: Default::default(),
                command_signatures: Default::default(),
                _cleanup: Dx12Cleanup,
            })
        }
    }

    fn with_window(args: &crate::Args, render_callback: RenderCallback) -> Result<Box<dyn Window>> {
        // Create a window and a swapchain, so tools can capture the dispatches as part of frames
        let backend = Self::new(args)?;

        unsafe {
            let instance =
                GetModuleHandleW(None).handle_err(&backend.device, "Get module handle")?;
            assert!(!instance.0.is_null());

            // Register a window class for the event handler
            let class = WNDCLASSEXW {
                cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_event_handler),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).handle_err(&backend.device, "Load cursor")?,
                lpszClassName: w!("smoldr"),

                ..Default::default()
            };

            let success = RegisterClassExW(&class);
            assert!(success != 0, "Failed to register window class");

            let mut title = String::from("smoldr");
            if let Some(file) = &args.filename {
                title.push(' ');
                write!(title, "{}", file.display()).unwrap();
            }
            let title = HSTRING::from(title);

            // Create window
            let window = CreateWindowExW(
                Default::default(),
                class.lpszClassName,
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                400,
                400,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .h_err(&backend, "Casting swap chain")?;

            // Create swap chain
            let desc = Dxgi::DXGI_SWAP_CHAIN_DESC1 {
                Width: 400,
                Height: 400,
                Format: Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, ..Default::default() },
                BufferUsage: Dxgi::DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: RENDER_TARGETS,
                SwapEffect: Dxgi::DXGI_SWAP_EFFECT_FLIP_DISCARD,
                Flags: Dxgi::DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32,
                ..Default::default()
            };
            let fs_desc = Dxgi::DXGI_SWAP_CHAIN_FULLSCREEN_DESC {
                Windowed: true.into(),
                ..Default::default()
            };

            let factory: Dxgi::IDXGIFactory2 =
                Dxgi::CreateDXGIFactory().context("Creating dxgi factory")?;
            let swap_chain: Dxgi::IDXGISwapChain3 = factory
                .CreateSwapChainForHwnd(&backend.command_queue, window, &desc, Some(&fs_desc), None)
                .h_err(&backend, "Creating swap chain")?
                .cast()
                .h_err(&backend, "Casting swap chain")?;

            // Create descriptor heap for render target views
            let desc = D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                NumDescriptors: RENDER_TARGETS,
                ..Default::default()
            };
            let render_target_heap: ID3D12DescriptorHeap = backend
                .device
                .CreateDescriptorHeap(&desc)
                .h_err(&backend, "Creating render target descriptor heap")?;

            let mut window = Dx12Window {
                swap_chain,
                back_buffer_views: Default::default(),
                render_target_heap,
                window,
                backend,
                render_callback,
            };
            window.create_back_buffer_views()?;
            Ok(Box::new(window))
        }
    }

    fn devices() -> Result<Vec<String>>
    where Self: Sized {
        let mut res = Vec::new();
        unsafe {
            let factory: Dxgi::IDXGIFactory =
                Dxgi::CreateDXGIFactory().context("Creating dxgi factory")?;
            let mut i = 0;
            while let Ok(adapter) = factory.EnumAdapters(i) {
                res.push(Self::get_adapter_desc(&adapter)?);
                i += 1;
            }
        }
        Ok(res)
    }

    fn messages(&self) -> Result<String> {
        if self.debug_controller.is_some() {
            Self::messages(&self.device)
        } else {
            Ok(String::new())
        }
    }

    fn reset(&mut self) {
        self.descriptor_heap_size = 0;
        self.objects.clear();
        self.buffers.clear();
        self.root_sigs.clear();
        self.shader_ids.clear();
        self.shader_tables.clear();
        self.pipelines.clear();
        self.psos.clear();
        self.views.clear();
        self.tlas.clear();
        self.blas.clear();
        self.command_signatures.clear();
    }

    fn compile(
        &mut self, id: IdentifierIdx, source: &str, path: &Path, dir: &Directive,
    ) -> Result<()> {
        let Directive::Object { shader_model, entrypoint, args, .. } = dir else { unreachable!() };

        unsafe {
            #[cfg(feature = "enable_cpp")]
            let mut object = None;
            #[cfg(not(feature = "enable_cpp"))]
            let object = None;

            let mut args = args.iter().map(HSTRING::from).collect::<Vec<_>>();
            if let Some(p) = path.parent().and_then(|p| p.to_str()) {
                args.push(HSTRING::from("-I"));
                args.push(HSTRING::from(p));
            }
            trace!(?args, "Dxc compilation arguments");

            // Use C++ to compile a shader
            // For demonstration purpose
            #[cfg(feature = "enable_cpp")]
            {
                use windows::core::Vtable;

                let compiler =
                    Dxc::DxcCreateInstance::<Dxc::IDxcCompiler3>(&Dxc::CLSID_DxcCompiler)
                        .context("Creating Dxc compiler")?;

                args.push(HSTRING::from("-T"));
                args.push(HSTRING::from(shader_model));
                if let Some(e) = entrypoint {
                    args.push(HSTRING::from("-E"));
                    args.push(to_str(e));
                }

                let args = args.iter().map(|s| s.as_ptr()).collect::<Vec<_>>();
                let obj = ffi::compile(compiler.into_raw().cast(), source, &arg_refs)
                    .context("Compile HLSL")?;
                let compile_res = Dxc::IDxcOperationResult::from_raw(obj.cast());
                object = Self::create_object_from_result(compile_res)?;
            }

            let obj;
            if let Some(object) = object {
                obj = object;
            } else {
                // Use rust to compile a shader

                // WIN-FIXME Fix in win32metadata: Add a uuid for DxcUtils, there is one in C++
                let utils = Dxc::DxcCreateInstance::<Dxc::IDxcUtils>(&Dxc::CLSID_DxcLibrary)
                    .h_err(self, "Creating Dxc utils")?;
                // WIN-FIXME Wait for next release to use IDxcCompiler3
                let compiler =
                    Dxc::DxcCreateInstance::<Dxc::IDxcCompiler2>(&Dxc::CLSID_DxcCompiler)
                        .h_err(self, "Creating Dxc compiler")?;
                let source = utils
                    .CreateBlob(
                        source.as_ptr().cast(),
                        source.len().try_into().context("Source too long")?,
                        Dxc::DXC_CP_UTF8,
                    )
                    .h_err(self, "Create HLSL source blob")?;

                let args = args.iter().map(|s| PCWSTR(s.as_ptr())).collect::<Vec<_>>();

                let include_handler =
                    utils.CreateDefaultIncludeHandler().h_err(self, "Create include handler")?;

                let compile_res = compiler
                    .Compile(
                        &source,
                        w!("source.hlsl"),
                        &HSTRING::from(entrypoint.as_deref().unwrap_or_default()),
                        &HSTRING::from(shader_model),
                        Some(&args),
                        &[],
                        Some(&include_handler),
                    )
                    .h_err(self, "Compile HLSL")?;

                obj = Self::create_object_from_result(compile_res)?;
            }
            self.objects.insert(id, obj);
        }
        Ok(())
    }

    fn compile_dxil(&mut self, id: IdentifierIdx, source: &str, _: &Directive) -> Result<()> {
        unsafe {
            // WIN-FIXME Fix in win32metadata: Add a uuid for DxcUtils, there is one in C++
            let utils = Dxc::DxcCreateInstance::<Dxc::IDxcUtils>(&Dxc::CLSID_DxcLibrary)
                .h_err(self, "Creating Dxc utils")?;
            let compiler = Dxc::DxcCreateInstance::<Dxc::IDxcAssembler>(&Dxc::CLSID_DxcAssembler)
                .h_err(self, "Creating Dxc assembler")?;
            let source = utils
                .CreateBlob(
                    source.as_ptr().cast(),
                    source.len().try_into().context("Source too long")?,
                    Dxc::DXC_CP_UTF8,
                )
                .h_err(self, "Create DXIL source blob")?;

            let compile_res = compiler.AssembleToContainer(&source).h_err(self, "Assemble DXIL")?;

            let object = Self::create_object_from_result(compile_res)?;
            self.objects.insert(id, object);
        }
        Ok(())
    }

    fn disassemble_dxil(&mut self, object: IdentifierIdx, _: &Directive) -> Result<String> {
        unsafe {
            let compiler = Dxc::DxcCreateInstance::<Dxc::IDxcCompiler>(&Dxc::CLSID_DxcCompiler)
                .h_err(self, "Creating Dxc compiler")?;
            let assembly_blob = compiler
                .Disassemble(Some(&self.objects[&object]))
                .h_err(self, "Disassemble DXIL")?;
            let assembly = slice::from_raw_parts(
                assembly_blob.GetBufferPointer().cast::<u8>(),
                assembly_blob.GetBufferSize(),
            );
            Ok(str::from_utf8(assembly).context("Converting DXIL to UTF-8")?.to_string())
        }
    }

    fn create_blas(&mut self, id: IdentifierIdx, dir: &Directive) -> Result<()> {
        let Directive::Blas { name, procedurals, triangles, config } = dir else { unreachable!() };
        self.supports_raytracing()?;

        let f32_size = mem::size_of::<f32>();
        let i32_size = mem::size_of::<i32>();
        let vertex_stride = 3 * f32_size;
        let vertex_count = triangles.iter().map(|t| t.vertices.len()).sum::<usize>();
        let vertex_byte_size = (vertex_count * vertex_stride) as u64;

        let indices = false;

        let index_count = if indices { vertex_count } else { 0 };
        let index_byte_size = (index_count * i32_size) as u64;
        let transform_count = triangles.iter().filter_map(|t| t.transform.as_ref()).count();
        let transform_byte_size = (transform_count * mem::size_of::<Transform>()) as u64;
        let aabb_count = procedurals.iter().map(|p| p.aabbs.len()).sum::<usize>();
        let aabb_byte_size = (aabb_count * mem::size_of::<Aabb>()) as u64;

        let mut geometries = Vec::new();

        // Buffer layout is
        // transforms, aabbs, vertices, indices
        let all_byte_size =
            transform_byte_size + aabb_byte_size + vertex_byte_size + index_byte_size;
        let vertex_buffer = self.create_buffer_intern(
            all_byte_size,
            u64::from(
                D3D12_RAYTRACING_TRANSFORM3X4_BYTE_ALIGNMENT
                    .max(D3D12_RAYTRACING_AABB_BYTE_ALIGNMENT),
            ),
            D3D12_HEAP_TYPE_DEFAULT,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_COMMON,
            &format!("BLAS {name} triangle vertex buffer"),
        )?;
        let buffer_addr = unsafe { vertex_buffer.GetGPUVirtualAddress() };

        // Triangles
        self.upload_intern(&vertex_buffer, &mut |data| {
            let transform_start = 0;
            let aabb_start = transform_start + usize::try_from(transform_byte_size).unwrap();
            let vertex_start = aabb_start + usize::try_from(aabb_byte_size).unwrap();
            let index_start = vertex_start + usize::try_from(vertex_byte_size).unwrap();

            let mut transform_offset = transform_start;
            let mut aabb_offset = aabb_start;
            let mut vertex_offset = vertex_start;
            let mut index_offset = index_start;

            for t in triangles {
                let transform = if let Some(trans) = &t.transform {
                    let o = transform_offset;
                    // Upload transform
                    for i in &trans.0 {
                        data[transform_offset..transform_offset + f32_size]
                            .copy_from_slice(&i.to_le_bytes());
                        transform_offset += f32_size;
                    }

                    buffer_addr + o as u64
                } else {
                    0
                };

                let geometry = D3D12_RAYTRACING_GEOMETRY_DESC {
                    Type: D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES,
                    Flags: D3D12_RAYTRACING_GEOMETRY_FLAGS(t.config.bits() as i32),
                    Anonymous: D3D12_RAYTRACING_GEOMETRY_DESC_0 {
                        Triangles: D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
                            Transform3x4: transform,
                            IndexFormat: if indices {
                                dxgi::DXGI_FORMAT_R32_UINT
                            } else {
                                dxgi::DXGI_FORMAT_UNKNOWN
                            },
                            VertexFormat: dxgi::DXGI_FORMAT_R32G32B32_FLOAT,
                            IndexCount: if indices {
                                u32::try_from(t.vertices.len()).unwrap()
                            } else {
                                0
                            },
                            VertexCount: u32::try_from(t.vertices.len()).unwrap(),
                            IndexBuffer: if indices {
                                buffer_addr + index_offset as u64
                            } else {
                                0
                            },
                            VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE {
                                StartAddress: buffer_addr + vertex_offset as u64,
                                StrideInBytes: vertex_stride as u64,
                            },
                        },
                    },
                };
                geometries.push(geometry);

                // Upload vertices
                for (index, v) in t.vertices.iter().enumerate() {
                    for i in &v.0 {
                        data[vertex_offset..vertex_offset + f32_size]
                            .copy_from_slice(&i.to_le_bytes());
                        vertex_offset += f32_size;
                    }

                    if indices {
                        // Upload indices
                        data[index_offset..index_offset + i32_size]
                            .copy_from_slice(&u32::try_from(index).unwrap().to_le_bytes());
                        index_offset += i32_size;
                    }
                }
            }

            debug_assert_eq!(
                (transform_offset - transform_start) as u64,
                transform_byte_size,
                "Incorrect size computation"
            );

            debug_assert_eq!(
                (vertex_offset - vertex_start) as u64,
                vertex_byte_size,
                "Incorrect size computation"
            );

            debug_assert_eq!(
                (index_offset - index_start) as u64,
                index_byte_size,
                "Incorrect size computation"
            );

            for p in procedurals {
                let geometry = D3D12_RAYTRACING_GEOMETRY_DESC {
                    Type: D3D12_RAYTRACING_GEOMETRY_TYPE_PROCEDURAL_PRIMITIVE_AABBS,
                    Flags: D3D12_RAYTRACING_GEOMETRY_FLAGS(p.config.bits() as i32),
                    Anonymous: D3D12_RAYTRACING_GEOMETRY_DESC_0 {
                        AABBs: D3D12_RAYTRACING_GEOMETRY_AABBS_DESC {
                            AABBCount: p.aabbs.len() as u64,
                            AABBs: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE {
                                StartAddress: buffer_addr + aabb_offset as u64,
                                StrideInBytes: 6 * f32_size as u64,
                            },
                        },
                    },
                };
                geometries.push(geometry);

                // Upload aabbs
                for b in &p.aabbs {
                    for i in b.min.0.iter().chain(b.max.0.iter()) {
                        data[aabb_offset..aabb_offset + f32_size].copy_from_slice(&i.to_le_bytes());
                        aabb_offset += f32_size;
                    }
                }
            }

            debug_assert_eq!(
                (aabb_offset - aabb_start) as u64,
                aabb_byte_size,
                "Incorrect size computation"
            );

            debug_assert_eq!(transform_offset, aabb_start, "Incorrect offset computation");
            debug_assert_eq!(aabb_offset, vertex_start, "Incorrect offset computation");
            debug_assert_eq!(vertex_offset, index_start, "Incorrect offset computation");
            debug_assert_eq!(index_offset as u64, all_byte_size, "Incorrect offset computation");
        })?;

        let inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
            Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
            Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS(config.bits() as i32),
            NumDescs: u32::try_from(geometries.len()).context("Too many geometries")?,
            DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
            Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 {
                pGeometryDescs: geometries.as_ptr(),
            },
        };

        let buffer = self.build_accel_struct_intern(&name.content, inputs)?;
        self.blas.insert(id, buffer);

        Ok(())
    }

    fn create_tlas(
        &mut self, id: IdentifierIdx, blas_ids: &[IdentifierIdx], dir: &Directive,
    ) -> Result<()> {
        let Directive::Tlas { name, blas, config } = dir else { unreachable!() };
        self.supports_raytracing()?;

        let mut identity_trans = Transform::default();
        identity_trans.0[0] = 1.0;
        identity_trans.0[5] = 1.0;
        identity_trans.0[10] = 1.0;
        let identity_trans = identity_trans;
        let el_size = mem::size_of::<D3D12_RAYTRACING_INSTANCE_DESC>();
        // WIN-FIXME D3D12_RAYTRACING_INSTANCE_DESC_BYTE_ALIGNMENT
        let el_padded_size = next_multiple_of(el_size, 16);

        let blas_byte_size = (blas.len() * el_padded_size) as u64;

        let instance_desc_buffer = self.create_buffer_intern(
            blas_byte_size,
            u64::from(D3D12_RAYTRACING_INSTANCE_DESCS_BYTE_ALIGNMENT),
            D3D12_HEAP_TYPE_DEFAULT,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_COMMON,
            &format!("TLAS {name} instances"),
        )?;

        unsafe {
            self.upload_intern(&instance_desc_buffer, &mut |data| {
                let mut offset = 0;
                for (id, b) in blas_ids.iter().zip(blas.iter()) {
                    let transform = if let Some(t) = &b.transform { t } else { &identity_trans };
                    let flags = b.config.bits();

                    let desc = D3D12_RAYTRACING_INSTANCE_DESC {
                        Transform: transform.0,
                        _bitfield1: b.id.unwrap_or_default()
                            | ((b.mask.unwrap_or(0xff) as u32) << 24),
                        _bitfield2: b.index_contrib.unwrap_or_default() | (flags << 24),
                        AccelerationStructure: self.blas[id].GetGPUVirtualAddress(),
                    };

                    // Upload instance
                    let bytes = slice::from_raw_parts(
                        (&desc as *const D3D12_RAYTRACING_INSTANCE_DESC).cast(),
                        el_size,
                    );
                    data[offset..offset + el_size].copy_from_slice(bytes);

                    offset += el_padded_size;
                }
            })?;

            let inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
                Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
                Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS(config.bits() as i32),
                NumDescs: u32::try_from(blas.len()).context("Too many blas")?,
                DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 {
                    InstanceDescs: instance_desc_buffer.GetGPUVirtualAddress(),
                },
            };

            let buffer = self.build_accel_struct_intern(&name.content, inputs)?;
            self.tlas.insert(id, buffer);
        }

        Ok(())
    }

    fn create_buffer(&mut self, id: IdentifierIdx, dir: &Directive) -> Result<()> {
        let Directive::Buffer { name, content, .. } = dir else { unreachable!() };

        let buffer = self.create_buffer_intern(
            u64::try_from(content.len()).unwrap(),
            0,
            D3D12_HEAP_TYPE_DEFAULT,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_COMMON,
            &name.content,
        )?;

        self.buffers.insert(id, buffer);
        Ok(())
    }

    fn create_root_sig(&mut self, id: IdentifierIdx, dir: &Directive) -> Result<()> {
        let Directive::RootSig { name, entries, config } = dir else { unreachable!() };

        let mut ranges = Vec::new();
        let mut params = Vec::new();

        for e in entries {
            match e {
                RootSigEntry::Table(RootSigTable { typ, register, number, space }) => {
                    let typ = match typ {
                        ViewType::Srv => D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
                        ViewType::Uav => D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
                    };
                    let range = D3D12_DESCRIPTOR_RANGE1 {
                        RangeType: typ,
                        NumDescriptors: *number,
                        BaseShaderRegister: *register,
                        RegisterSpace: *space,
                        ..Default::default()
                    };
                    let range = Box::new(range);

                    params.push(D3D12_ROOT_PARAMETER1 {
                        ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                        Anonymous: D3D12_ROOT_PARAMETER1_0 {
                            DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE1 {
                                NumDescriptorRanges: 1,
                                pDescriptorRanges: &*range,
                            },
                        },
                        ..Default::default()
                    });
                    ranges.push(range);
                }
                RootSigEntry::View(RootSigView { typ, register, space }) => {
                    let typ = match typ {
                        ViewType::Srv => D3D12_ROOT_PARAMETER_TYPE_SRV,
                        ViewType::Uav => D3D12_ROOT_PARAMETER_TYPE_UAV,
                    };

                    params.push(D3D12_ROOT_PARAMETER1 {
                        ParameterType: typ,
                        Anonymous: D3D12_ROOT_PARAMETER1_0 {
                            Descriptor: D3D12_ROOT_DESCRIPTOR1 {
                                ShaderRegister: *register,
                                RegisterSpace: *space,
                                ..Default::default()
                            },
                        },
                        ..Default::default()
                    });
                }
                RootSigEntry::Const(RootSigConst { number: count, register, space }) => {
                    params.push(D3D12_ROOT_PARAMETER1 {
                        ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                        Anonymous: D3D12_ROOT_PARAMETER1_0 {
                            Constants: D3D12_ROOT_CONSTANTS {
                                ShaderRegister: *register,
                                RegisterSpace: *space,
                                Num32BitValues: *count,
                            },
                        },
                        ..Default::default()
                    });
                }
            }
        }

        let sig = D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
            Version: D3D_ROOT_SIGNATURE_VERSION_1_1,
            Anonymous: D3D12_VERSIONED_ROOT_SIGNATURE_DESC_0 {
                Desc_1_1: D3D12_ROOT_SIGNATURE_DESC1 {
                    NumParameters: params
                        .len()
                        .try_into()
                        .context("Too many root signature parameters")?,
                    pParameters: params.as_ptr(),
                    Flags: D3D12_ROOT_SIGNATURE_FLAGS(config.bits() as i32),
                    ..Default::default()
                },
            },
        };

        unsafe {
            let mut blob = None;
            let mut error_blob = None;
            // WIN-FIXME Should return error and blob?
            D3D12SerializeVersionedRootSignature(&sig, &mut blob, Some(&mut error_blob))
                .h_err(self, "Serializing root signature")?;

            let blob = blob.ok_or_else(|| miette!("Failed to create root signature"))?;
            let root_sig_data =
                slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize());
            let root_sig: ID3D12RootSignature = self
                .device
                .CreateRootSignature(0, root_sig_data)
                .h_err(self, "Creating root signature")?;
            root_sig
                .SetName(&HSTRING::from(&name.content))
                .h_err(self, "Set root signature name")?;
            self.root_sigs.insert(id, root_sig);
        }
        Ok(())
    }

    fn create_root_sig_dxil(
        &mut self, id: IdentifierIdx, object: IdentifierIdx, dir: &Directive,
    ) -> Result<()> {
        let Directive::RootSigDxil { name, .. } = dir else { unreachable!() };

        unsafe {
            let object = &self.objects[&object];
            let data =
                slice::from_raw_parts(object.GetBufferPointer().cast(), object.GetBufferSize());
            let root_sig: ID3D12RootSignature = self
                .device
                .CreateRootSignature(0, data)
                .h_err(self, "Creating root signature from dxil")?;
            root_sig
                .SetName(&HSTRING::from(&name.content))
                .h_err(self, "Set root signature name")?;
            self.root_sigs.insert(id, root_sig);
        }
        Ok(())
    }

    fn create_shader_id(
        &mut self, id: IdentifierIdx, pso: IdentifierIdx, dir: &Directive,
    ) -> Result<()> {
        let Directive::ShaderId { shader_name, .. } = dir else { unreachable!() };

        let shader_id = self.get_pso_shader_id(pso, shader_name)?;
        self.shader_ids.insert(id, shader_id);
        Ok(())
    }

    fn create_shader_table(
        &mut self, id: IdentifierIdx, pso: IdentifierIdx, root_vals: &[RootVal],
        shader_ids: &[Option<IdentifierIdx>], dir: &Directive,
    ) -> Result<()> {
        let Directive::ShaderTable { name, records, .. } = dir else { unreachable!() };

        let stride = self.get_local_root_sig_stride(records);
        let rec_count = records.iter().map(|r| r.index + 1).max().unwrap_or_default();
        if stride > D3D12_RAYTRACING_MAX_SHADER_RECORD_STRIDE as usize {
            return Err(error::TooManyLocalRootEntries {
                name: name.clone(),
                stride,
                max_size: D3D12_RAYTRACING_MAX_SHADER_RECORD_STRIDE as usize,
            }
            .into());
        }

        let buf = self.create_buffer_intern(
            u64::from(rec_count) * stride as u64,
            u64::from(D3D12_RAYTRACING_SHADER_TABLE_BYTE_ALIGNMENT),
            D3D12_HEAP_TYPE_DEFAULT,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_COMMON,
            &format!("Shader table buffer {name}"),
        )?;

        let fill = |data: &mut [u8]| {
            for (i, ((rec, shader), root)) in
                records.iter().zip(shader_ids.iter()).zip(root_vals.iter()).enumerate()
            {
                let part = &mut data[i * stride..(i + 1) * stride];

                // Copy shader id
                let shader_id;
                let shader = match &rec.shader {
                    ShaderReference::None => {
                        shader_id = Default::default();
                        &shader_id
                    }
                    ShaderReference::ShaderId(_) => &self.shader_ids[&shader.unwrap()],
                    ShaderReference::Name(n) => {
                        shader_id = self.get_pso_shader_id(pso, n)?;
                        &shader_id
                    }
                };
                part[..shader.len()].copy_from_slice(shader);
                let part = &mut part[shader.len()..];

                // Copy root signature
                let el_size = mem::size_of::<u64>();
                // Views
                for (v, v_id) in rec.root_val.binds.iter().zip(root.binds.iter()) {
                    let part = &mut part[v.index as usize * el_size..];
                    let data = self.views[v_id].gpu.ptr.to_le_bytes();
                    part[..data.len()].copy_from_slice(&data);
                }

                // Buffers
                for (v, v_id) in rec.root_val.views.iter().zip(root.views.iter()) {
                    let part = &mut part[v.index as usize * el_size..];
                    let data = unsafe { self.buffers[v_id].GetGPUVirtualAddress().to_le_bytes() };
                    part[..data.len()].copy_from_slice(&data);
                }

                // Consts
                for c in &root.consts {
                    let part = &mut part[c.index as usize * el_size..];
                    let len = c.content.len();
                    c.content.fill(&mut part[..len]);
                }
            }
            Ok(())
        };

        let mut error = None;
        self.upload_intern(&buf, &mut |data| {
            if let Err(e) = fill(data) {
                error = Some(e);
            }
        })?;

        if let Some(e) = error {
            return Err(e);
        }

        unsafe {
            buf.SetName(&HSTRING::from(&name.content)).h_err(self, "Set buffer name")?;
            self.shader_tables.insert(id, (buf, stride));
        }
        Ok(())
    }

    fn create_compute_pipeline(
        &mut self, id: IdentifierIdx, shader: IdentifierIdx, root_sig: Option<IdentifierIdx>,
        dir: &Directive,
    ) -> Result<()> {
        let Directive::Pipeline { name, .. } = dir else { unreachable!() };

        unsafe {
            let shader = &self.objects[&shader];
            let root_sig_ref = if let Some(r) = root_sig {
                std::mem::transmute_copy(&self.root_sigs[&r])
            } else {
                ManuallyDrop::new(None)
            };
            let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: root_sig_ref,
                CS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: shader.GetBufferPointer(),
                    BytecodeLength: shader.GetBufferSize(),
                },
                ..Default::default()
            };

            let pipeline: ID3D12PipelineState = self
                .device
                .CreateComputePipelineState(&desc)
                .h_err(self, "Creating compute pipeline")?;

            pipeline
                .SetName(&HSTRING::from(&name.content))
                .h_err(self, "Set compute pipeline name")?;
            self.pipelines.insert(id, (pipeline, root_sig));
        }
        Ok(())
    }

    fn create_pipeline_state_object(
        &mut self, id: IdentifierIdx, add_to: Option<IdentifierIdx>, lib_ids: &[IdentifierIdx],
        collection_ids: &[IdentifierIdx], dir: &Directive,
    ) -> Result<()> {
        let Directive::PipelineStateObject {
            name, typ, libs, collections, hit_groups, config, ..
        } = dir
        else {
            unreachable!()
        };

        self.supports_raytracing()?;
        let typ = match typ {
            PipelineStateObjectType::Pipeline => D3D12_STATE_OBJECT_TYPE_RAYTRACING_PIPELINE,
            PipelineStateObjectType::Collection => D3D12_STATE_OBJECT_TYPE_COLLECTION,
        };

        // Save objects we take pointers from
        let mut subobjs = Vec::new();
        let mut lib_descs = Vec::new();
        let mut coll_descs = Vec::new();
        let mut group_descs = Vec::new();
        let mut export_descs = Vec::new();
        let mut strings = Vec::new();

        let mut map_exports = |exports: &[Export]| {
            exports
                .iter()
                .map(|e| {
                    let name = HSTRING::from(&e.name);
                    let to_rename = e
                        .to_rename
                        .as_ref()
                        .map(|s| {
                            let s = HSTRING::from(s);
                            let r = PCWSTR(s.as_ptr());
                            strings.push(s);
                            r
                        })
                        .unwrap_or(PCWSTR::null());

                    let res = D3D12_EXPORT_DESC {
                        Name: PCWSTR(name.as_ptr()),
                        ExportToRename: to_rename,
                        ..Default::default()
                    };
                    strings.push(name);
                    res
                })
                .collect::<Vec<_>>()
        };

        unsafe {
            // DXIL libs
            for (obj, id) in libs.iter().zip(lib_ids.iter()) {
                let mut exports = map_exports(&obj.exports);

                let dxil = &self.objects[id];
                let obj = Box::new(D3D12_DXIL_LIBRARY_DESC {
                    DXILLibrary: D3D12_SHADER_BYTECODE {
                        pShaderBytecode: dxil.GetBufferPointer().cast(),
                        BytecodeLength: dxil.GetBufferSize(),
                    },
                    NumExports: u32::try_from(exports.len()).context("Too many exports")?,
                    // WIN-FIXME Why is this a mut ptr?
                    pExports: exports.as_mut_ptr(),
                });
                export_descs.push(exports);
                subobjs.push(D3D12_STATE_SUBOBJECT {
                    Type: D3D12_STATE_SUBOBJECT_TYPE_DXIL_LIBRARY,
                    pDesc: (&*obj as *const D3D12_DXIL_LIBRARY_DESC).cast(),
                });
                lib_descs.push(obj);
            }

            // Collections
            for (obj, id) in collections.iter().zip(collection_ids.iter()) {
                let mut exports = map_exports(&obj.exports);

                let obj = Box::new(D3D12_EXISTING_COLLECTION_DESC {
                    pExistingCollection: std::mem::transmute_copy(&self.psos[id]),
                    NumExports: u32::try_from(exports.len()).context("Too many exports")?,
                    pExports: exports.as_mut_ptr(),
                });
                export_descs.push(exports);
                subobjs.push(D3D12_STATE_SUBOBJECT {
                    Type: D3D12_STATE_SUBOBJECT_TYPE_EXISTING_COLLECTION,
                    pDesc: (&*obj as *const D3D12_EXISTING_COLLECTION_DESC).cast(),
                });
                coll_descs.push(obj);
            }

            // Hit groups
            for group in hit_groups {
                let name = HSTRING::from(&group.name.content);
                let mut shader_names = group.shaders.iter().map(|s| {
                    s.as_ref()
                        .map(|s| {
                            let s = HSTRING::from(&s.content);
                            let sref = PCWSTR(s.as_ptr());
                            strings.push(s);
                            sref
                        })
                        .unwrap_or(PCWSTR(ptr::null()))
                });

                let obj = Box::new(D3D12_HIT_GROUP_DESC {
                    HitGroupExport: PCWSTR(name.as_ptr()),
                    Type: if group.shaders[2].is_some() {
                        D3D12_HIT_GROUP_TYPE_PROCEDURAL_PRIMITIVE
                    } else {
                        D3D12_HIT_GROUP_TYPE_TRIANGLES
                    },
                    AnyHitShaderImport: shader_names.next().unwrap(),
                    ClosestHitShaderImport: shader_names.next().unwrap(),
                    IntersectionShaderImport: shader_names.next().unwrap(),
                });
                subobjs.push(D3D12_STATE_SUBOBJECT {
                    Type: D3D12_STATE_SUBOBJECT_TYPE_HIT_GROUP,
                    pDesc: (&*obj as *const D3D12_HIT_GROUP_DESC).cast(),
                });
                strings.push(name);
                group_descs.push(obj);
            }

            let obj_config;
            if !config.is_empty() {
                // Add config
                obj_config = Box::new(D3D12_STATE_OBJECT_CONFIG {
                    Flags: D3D12_STATE_OBJECT_FLAGS(config.bits() as i32),
                });
                subobjs.push(D3D12_STATE_SUBOBJECT {
                    Type: D3D12_STATE_SUBOBJECT_TYPE_STATE_OBJECT_CONFIG,
                    pDesc: (&*obj_config as *const D3D12_STATE_OBJECT_CONFIG).cast(),
                });
            }

            let desc = D3D12_STATE_OBJECT_DESC {
                Type: typ,
                NumSubobjects: u32::try_from(subobjs.len()).context("Too many subobjects")?,
                pSubobjects: subobjs.as_ptr(),
            };

            let pso: ID3D12StateObject = if let Some(add_to) = add_to {
                self.device
                    .AddToStateObject(&desc, &self.psos[&add_to])
                    .h_err(self, "Adding to pipeline state object")?
            } else {
                self.device
                    .CreateStateObject(&desc)
                    .h_err(self, "Creating pipeline state object")?
            };

            pso.SetName(&HSTRING::from(&name.content))
                .h_err(self, "Set pipeline state object name")?;
            self.psos.insert(id, pso);
        }
        Ok(())
    }

    fn create_view(
        &mut self, id: IdentifierIdx, buffer: Option<IdentifierIdx>, dir: &Directive,
    ) -> Result<()> {
        let Directive::View { name, typ: view_type, .. } = dir else { unreachable!() };

        unsafe {
            let view = self.descriptor_handles()?;

            match view_type {
                InputViewType::RaytracingAccelStruct => {
                    let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
                        Format: dxgi::DXGI_FORMAT_UNKNOWN,
                        ViewDimension: D3D12_SRV_DIMENSION_RAYTRACING_ACCELERATION_STRUCTURE,
                        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                            RaytracingAccelerationStructure:
                                D3D12_RAYTRACING_ACCELERATION_STRUCTURE_SRV {
                                    Location: buffer
                                        .map(|b| self.tlas[&b].GetGPUVirtualAddress())
                                        .unwrap_or_default(),
                                },
                        },
                    };

                    self.device.CreateShaderResourceView(None, Some(&desc), view.cpu);
                    self.views.insert(id, view);
                }
                InputViewType::Raw { typ }
                | InputViewType::Typed { typ, .. }
                | InputViewType::Structured { typ, .. } => {
                    let buffer = buffer.unwrap();
                    let mut struct_stride = 0;
                    let (format, el_size) = match view_type {
                        InputViewType::Typed { data_type, .. } => match data_type {
                            DataType::U32 => (dxgi::DXGI_FORMAT_R32_UINT, mem::size_of::<u32>()),
                            DataType::U16 => (dxgi::DXGI_FORMAT_R16_UINT, mem::size_of::<u16>()),
                            DataType::U8 => (dxgi::DXGI_FORMAT_R8_UINT, mem::size_of::<u8>()),
                            DataType::F32 => (dxgi::DXGI_FORMAT_R32_FLOAT, mem::size_of::<f32>()),
                            DataType::F16 => (dxgi::DXGI_FORMAT_R16_FLOAT, mem::size_of::<f16>()),
                            DataType::U64 => {
                                return Err(error::InvalidDataType {
                                    declaration: name.clone(),
                                    typ: *data_type,
                                }
                                .into());
                            }
                        },
                        InputViewType::Raw { .. } => {
                            (dxgi::DXGI_FORMAT_R32_TYPELESS, mem::size_of::<u32>())
                        }
                        InputViewType::Structured { struct_size, .. } => {
                            struct_stride = *struct_size;
                            (dxgi::DXGI_FORMAT_UNKNOWN, *struct_size as usize)
                        }
                        InputViewType::RaytracingAccelStruct => unreachable!(),
                    };
                    let is_raw = matches!(view_type, InputViewType::Raw { .. });

                    let buffer_desc = self.buffers[&buffer].GetDesc();
                    let num_elements = (buffer_desc.Width / el_size as u64)
                        .try_into()
                        .context("Buffer too large")?;

                    match typ {
                        ViewType::Srv => {
                            let flags = if is_raw {
                                D3D12_BUFFER_SRV_FLAG_RAW
                            } else {
                                D3D12_BUFFER_SRV_FLAG_NONE
                            };

                            let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
                                Format: format,
                                ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
                                Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                                Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                                    Buffer: D3D12_BUFFER_SRV {
                                        FirstElement: 0,
                                        NumElements: num_elements,
                                        StructureByteStride: struct_stride,
                                        Flags: flags,
                                    },
                                },
                            };

                            self.device.CreateShaderResourceView(
                                &self.buffers[&buffer],
                                Some(&desc),
                                view.cpu,
                            );
                            self.views.insert(id, view);
                        }
                        ViewType::Uav => {
                            let flags = if is_raw {
                                D3D12_BUFFER_UAV_FLAG_RAW
                            } else {
                                D3D12_BUFFER_UAV_FLAG_NONE
                            };

                            let desc = D3D12_UNORDERED_ACCESS_VIEW_DESC {
                                Format: format,
                                ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
                                Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                                    Buffer: D3D12_BUFFER_UAV {
                                        FirstElement: 0,
                                        NumElements: num_elements,
                                        StructureByteStride: struct_stride,
                                        Flags: flags,
                                        ..Default::default()
                                    },
                                },
                            };

                            self.device.CreateUnorderedAccessView(
                                &self.buffers[&buffer],
                                None,
                                Some(&desc),
                                view.cpu,
                            );
                            self.views.insert(id, view);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn create_command_signature(
        &mut self, id: IdentifierIdx, root_sig: Option<IdentifierIdx>, dir: &Directive,
    ) -> Result<()> {
        let Directive::CommandSignature { name, stride, arguments, .. } = dir else {
            unreachable!()
        };
        unsafe {
            let mut args = Vec::new();
            for arg in arguments {
                let desc = match arg {
                    CommandSignatureArgument::Constant { index, number, offset } => {
                        assert_eq!(
                            *offset % 4,
                            0,
                            "offset must be a multiple of 4 but is {offset}",
                        );
                        D3D12_INDIRECT_ARGUMENT_DESC {
                            Type: D3D12_INDIRECT_ARGUMENT_TYPE_CONSTANT,
                            Anonymous: D3D12_INDIRECT_ARGUMENT_DESC_0 {
                                Constant: D3D12_INDIRECT_ARGUMENT_DESC_0_1 {
                                    RootParameterIndex: *index,
                                    DestOffsetIn32BitValues: *offset / 4,
                                    Num32BitValuesToSet: *number,
                                },
                            },
                        }
                    }
                    CommandSignatureArgument::View { typ, index } => {
                        let typ = match typ {
                            ViewType::Uav => D3D12_INDIRECT_ARGUMENT_TYPE_UNORDERED_ACCESS_VIEW,
                            ViewType::Srv => D3D12_INDIRECT_ARGUMENT_TYPE_SHADER_RESOURCE_VIEW,
                        };
                        D3D12_INDIRECT_ARGUMENT_DESC {
                            Type: typ,
                            Anonymous: D3D12_INDIRECT_ARGUMENT_DESC_0 {
                                ShaderResourceView: D3D12_INDIRECT_ARGUMENT_DESC_0_3 {
                                    RootParameterIndex: *index,
                                },
                            },
                        }
                    }
                    CommandSignatureArgument::Dispatch => D3D12_INDIRECT_ARGUMENT_DESC {
                        Type: D3D12_INDIRECT_ARGUMENT_TYPE_DISPATCH,
                        ..Default::default()
                    },
                    CommandSignatureArgument::DispatchRays => D3D12_INDIRECT_ARGUMENT_DESC {
                        Type: D3D12_INDIRECT_ARGUMENT_TYPE_DISPATCH_RAYS,
                        ..Default::default()
                    },
                };
                args.push(desc);
            }

            let size: usize = arguments
                .iter()
                .map(|a| match a {
                    CommandSignatureArgument::Constant { number, .. } => {
                        usize::try_from(*number).unwrap() * mem::size_of::<u32>()
                    }
                    CommandSignatureArgument::View { .. } => mem::size_of::<u64>(),
                    CommandSignatureArgument::Dispatch => {
                        mem::size_of::<D3D12_DISPATCH_ARGUMENTS>()
                    }
                    CommandSignatureArgument::DispatchRays => {
                        mem::size_of::<D3D12_DISPATCH_RAYS_DESC>()
                    }
                })
                .sum();

            let stride = stride.unwrap_or(u32::try_from(size).unwrap());
            // Check stride
            if usize::try_from(stride).unwrap() < size {
                return Err(error::TooSmallExecuteIndirectStride {
                    declaration: name.clone(),
                    stride,
                    need_stride: size,
                }
                .into());
            }

            let sig_desc = D3D12_COMMAND_SIGNATURE_DESC {
                ByteStride: stride,
                NumArgumentDescs: u32::try_from(args.len()).unwrap(),
                pArgumentDescs: args.as_ptr(),
                ..Default::default()
            };

            let root_sig = root_sig.map(|r| &self.root_sigs[&r]);
            let mut sig = None;
            self.device
                .CreateCommandSignature(&sig_desc, root_sig, &mut sig)
                .with_h_err(self, || format!("Creating command signature '{}'", name.content))?;

            let sig: ID3D12CommandSignature = sig
                .ok_or_else(|| miette!("Failed to create command signature '{}'", name.content))?;
            sig.SetName(&HSTRING::from(&name.content))
                .with_h_err(self, || format!("Set command signature name '{}'", name.content))?;

            self.command_signatures.insert(id, (sig, stride));
        }
        Ok(())
    }

    fn dispatch(
        &mut self, pipeline: IdentifierIdx, ids: &RootVal, content: DispatchContent,
        root_sig: Option<IdentifierIdx>, dir: &Directive,
    ) -> Result<Duration> {
        let Directive::Dispatch { identifier, root_val, typ, .. } = dir else { unreachable!() };

        unsafe {
            let cmds;
            let pipe_root_sig;
            match content {
                DispatchContent::Dispatch => {
                    let (pipeline, root) = &self.pipelines[&pipeline];
                    cmds = self.command_list_with_pipeline("dispatch", pipeline)?;
                    cmds.SetPipelineState(pipeline);
                    pipe_root_sig = *root;
                }
                DispatchContent::DispatchRays { .. } => {
                    let pso = &self.psos[&pipeline];
                    cmds = self.command_list("rt dispatch")?;
                    cmds.SetPipelineState1(pso);
                    pipe_root_sig = None;
                }
                DispatchContent::ExecuteIndirect { pipeline_kind, .. } => match pipeline_kind {
                    PipelineKind::Pipeline => {
                        let (pipeline, root) = &self.pipelines[&pipeline];
                        cmds = self.command_list_with_pipeline("indirect dispatch", pipeline)?;
                        cmds.SetPipelineState(pipeline);
                        pipe_root_sig = *root;
                    }
                    PipelineKind::PipelineStateObject => {
                        cmds = self.command_list("indirect dispatch")?;
                        let pso = &self.psos[&pipeline];
                        cmds.SetPipelineState1(pso);
                        pipe_root_sig = None;
                    }
                },
            }

            cmds.SetComputeRootSignature(root_sig.or(pipe_root_sig).map(|r| &self.root_sigs[&r]));
            if let Some(heap) = self.descriptor_heap.clone() {
                cmds.SetDescriptorHeaps(&[Some(heap)]);
            } else {
                cmds.SetDescriptorHeaps(&[]);
            }

            // Binds
            for (b, view) in root_val.binds.iter().zip(ids.binds.iter()) {
                cmds.SetComputeRootDescriptorTable(b.index, self.views[view].gpu);
            }

            // Views
            for (v, buffer) in root_val.views.iter().zip(ids.views.iter()) {
                match v.typ.unwrap() {
                    ViewType::Uav => {
                        cmds.SetComputeRootUnorderedAccessView(
                            v.index,
                            self.buffers[buffer].GetGPUVirtualAddress(),
                        );
                    }
                    ViewType::Srv => {
                        cmds.SetComputeRootShaderResourceView(
                            v.index,
                            self.buffers[buffer].GetGPUVirtualAddress(),
                        );
                    }
                }
            }

            // Root constants
            for c in &ids.consts {
                let mut buf = vec![0; c.content.len()];
                c.content.fill(&mut buf);
                cmds.SetComputeRoot32BitConstants(
                    c.index,
                    u32::try_from(buf.len() / mem::size_of::<u32>())
                        .context("Too many root constants")?,
                    buf.as_ptr().cast(),
                    0,
                );
            }

            let query_heap = self.get_query_heap()?.clone();
            cmds.EndQuery(&query_heap, D3D12_QUERY_TYPE_TIMESTAMP, 0);
            match content {
                DispatchContent::Dispatch => {
                    let DispatchType::Dispatch { dimensions } = typ else { unreachable!() };
                    cmds.Dispatch(dimensions.0[0], dimensions.0[1], dimensions.0[2]);
                }
                DispatchContent::DispatchRays { tables } => {
                    let DispatchType::DispatchRays { dimensions, .. } = typ else { unreachable!() };
                    let raygen = tables[0]
                        .map(|t| {
                            let t = &self.shader_tables[&t];
                            D3D12_GPU_VIRTUAL_ADDRESS_RANGE {
                                StartAddress: t.0.GetGPUVirtualAddress(),
                                SizeInBytes: t.0.GetDesc1().Width,
                            }
                        })
                        .unwrap_or_default();

                    // Everything but raygen
                    let mut tables = tables.iter().skip(1).map(|t| {
                        t.map(|t| {
                            let t = &self.shader_tables[&t];
                            D3D12_GPU_VIRTUAL_ADDRESS_RANGE_AND_STRIDE {
                                StartAddress: t.0.GetGPUVirtualAddress(),
                                SizeInBytes: t.0.GetDesc1().Width,
                                StrideInBytes: u64::try_from(t.1).unwrap(),
                            }
                        })
                        .unwrap_or_default()
                    });

                    let desc = D3D12_DISPATCH_RAYS_DESC {
                        RayGenerationShaderRecord: raygen,
                        MissShaderTable: tables.next().unwrap(),
                        HitGroupTable: tables.next().unwrap(),
                        CallableShaderTable: tables.next().unwrap(),
                        Width: dimensions.0[0],
                        Height: dimensions.0[1],
                        Depth: dimensions.0[2],
                    };
                    cmds.DispatchRays(&desc);
                }
                DispatchContent::ExecuteIndirect {
                    signature,
                    argument_buffer,
                    count_buffer,
                    ..
                } => {
                    let DispatchType::ExecuteIndirect {
                        signature: sig_name,
                        argument_buffer: arg_buffer_name,
                        argument_offset,
                        count_offset,
                        max_commands,
                        ..
                    } = typ
                    else {
                        unreachable!()
                    };

                    let signature = &self.command_signatures[&signature];
                    let argument_buffer = &self.buffers[&argument_buffer];
                    let argument_offset = argument_offset.unwrap_or_default();
                    let count_buffer = count_buffer.map(|b| (&self.buffers[&b]).into());

                    // Buffer size must be large enough for max commands
                    let buffer_desc = argument_buffer.GetDesc1();
                    let buffer_len = buffer_desc.Width * u64::from(buffer_desc.Height);
                    if argument_offset > buffer_len
                        || buffer_len - argument_offset
                            < u64::from(signature.1) * u64::from(*max_commands)
                    {
                        return Err(error::TooSmallExecuteIndirectBuffer {
                            buffer: arg_buffer_name.clone(),
                            execute_indirect: identifier.clone(),
                            signature: sig_name.clone(),
                            buffer_size: buffer_len,
                            offset: argument_offset,
                            max_commands: *max_commands,
                            stride: signature.1,
                        }
                        .into());
                    }

                    resource_barrier!(cmds(
                        argument_buffer,
                        D3D12_RESOURCE_STATE_COMMON,
                        D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
                    ));
                    if let Some(count_buffer) = count_buffer {
                        resource_barrier!(cmds(
                            count_buffer,
                            D3D12_RESOURCE_STATE_COMMON,
                            D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
                        ));
                    }

                    cmds.ExecuteIndirect(
                        &signature.0,
                        *max_commands,
                        argument_buffer,
                        argument_offset,
                        count_buffer,
                        count_offset.unwrap_or_default(),
                    );
                }
            }
            cmds.EndQuery(&query_heap, D3D12_QUERY_TYPE_TIMESTAMP, 1);
            cmds.ResolveQueryData(
                &query_heap,
                D3D12_QUERY_TYPE_TIMESTAMP,
                0,
                2,
                self.query_buffer.as_ref().unwrap(),
                0,
            );

            cmds.run(self)?;

            // Read back timestamps
            let query_buffer =
                MappedBuffer::<ReadOnlyAccess>::new(self.query_buffer.clone().unwrap())?;
            let start = u64::from_le_bytes(query_buffer[0..8].try_into().unwrap());
            let end = u64::from_le_bytes(query_buffer[8..16].try_into().unwrap());
            let elapsed_ticks = end - start;
            // Ticks / sec
            let freq = self
                .command_queue
                .GetTimestampFrequency()
                .h_err(self, "Get timestamp frequency")?;

            // Seconds and nanoseconds
            Ok(Duration::new(
                elapsed_ticks / freq,
                ((elapsed_ticks % freq) * 1_000_000_000 / freq)
                    .try_into()
                    .expect("Error when converting timestamp"),
            ))
        }
    }

    fn get_shader_id(&mut self, id: IdentifierIdx) -> Result<Vec<u8>> {
        Ok(self.shader_ids[&id].to_vec())
    }

    fn get_gpuva(&self, buffer: IdentifierIdx, typ: IdentifierType) -> Result<u64> {
        Ok(unsafe {
            match typ {
                IdentifierType::Buffer => self.buffers[&buffer].GetGPUVirtualAddress(),
                IdentifierType::ShaderTable => self.shader_tables[&buffer].0.GetGPUVirtualAddress(),
                IdentifierType::Tlas => self.tlas[&buffer].GetGPUVirtualAddress(),
                IdentifierType::Blas => self.blas[&buffer].GetGPUVirtualAddress(),
                _ => panic!("Unexpected type to get the gpuva"),
            }
        })
    }

    fn upload(&mut self, buffer_id: IdentifierIdx, f: &mut dyn FnMut(&mut [u8])) -> Result<()> {
        let buffer = self.buffers[&buffer_id].clone();
        self.upload_intern(&buffer, f)
    }

    fn download(&mut self, buffer_id: IdentifierIdx) -> Result<Vec<u8>> {
        unsafe {
            let buffer = &self.buffers[&buffer_id];
            let buffer_desc = buffer.GetDesc1();
            let size = buffer_desc.Width * u64::from(buffer_desc.Height);

            let download_buffer = self.create_buffer_intern(
                size,
                0,
                D3D12_HEAP_TYPE_READBACK,
                D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE,
                D3D12_RESOURCE_STATE_COPY_DEST,
                "Download buffer",
            )?;

            let buffer = &self.buffers[&buffer_id];

            let cmds = self.command_list("download")?;

            resource_barrier!(cmds(
                buffer,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            ));

            cmds.CopyResource(&download_buffer, buffer);
            cmds.run(self)?;

            let download = MappedBuffer::<ReadOnlyAccess>::new(download_buffer)?;
            Ok(download.deref().into())
        }
    }
}

impl Drop for Dx12Cleanup {
    fn drop(&mut self) {
        if let Ok(mut lock) = DEVICE.lock() {
            *lock = None;
        }
    }
}
