// Copyright (c) Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::Path;
use std::time::Duration;

use miette::Result;

use crate::{Directive, DispatchContent, IdentifierIdx, IdentifierType, RootVal};

#[cfg(feature = "enable_dx12")]
mod dx12;
mod null;

#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) enum BackendType {
    #[cfg(feature = "enable_dx12")]
    #[default]
    Dx12,
    #[cfg_attr(not(feature = "enable_dx12"), default)]
    Null,
}

/// Result of `RenderCallback`
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Continue {
    /// Continue main loop
    Continue,
    /// Exit main loop
    Exit,
}

/// Render function that is called once per frame.
pub(crate) type RenderCallback = Box<dyn FnMut(&mut dyn Backend) -> Continue>;

/// A window wraps a backend and is able to run frames.
pub(crate) trait Window {
    /// Window main loop that calls the `render_callback`.
    fn main_loop(&mut self) -> Result<()>;
}

pub(crate) trait Backend {
    fn new(args: &crate::Args) -> Result<Self>
    where Self: Sized;
    /// Create a new backend with a window to run a script one time per frame.
    fn with_window(args: &crate::Args, render_callback: RenderCallback) -> Result<Box<dyn Window>>
    where Self: Sized;

    fn messages(&self) -> Result<String> { Ok(String::new()) }
    /// Reset everything to the state it is after `new`.
    ///
    /// Allows re-running statements without re-creating the backend.
    fn reset(&mut self) {}

    fn devices() -> Result<Vec<String>>
    where Self: Sized {
        Ok(Vec::new())
    }

    fn compile(
        &mut self, _id: IdentifierIdx, _source: &str, _path: &Path, _dir: &Directive,
    ) -> Result<()> {
        Ok(())
    }
    fn compile_dxil(&mut self, _id: IdentifierIdx, _source: &str, _dir: &Directive) -> Result<()> {
        Ok(())
    }
    fn disassemble_dxil(&mut self, _object: IdentifierIdx, _dir: &Directive) -> Result<String> {
        Ok(String::new())
    }
    fn create_blas(&mut self, _id: IdentifierIdx, _dir: &Directive) -> Result<()> { Ok(()) }
    fn create_tlas(
        &mut self, _id: IdentifierIdx, _blas: &[IdentifierIdx], _dir: &Directive,
    ) -> Result<()> {
        Ok(())
    }
    fn create_buffer(&mut self, _id: IdentifierIdx, _dir: &Directive) -> Result<()> { Ok(()) }
    fn create_root_sig(&mut self, _id: IdentifierIdx, _dir: &Directive) -> Result<()> { Ok(()) }
    fn create_root_sig_dxil(
        &mut self, _id: IdentifierIdx, _object: IdentifierIdx, _dir: &Directive,
    ) -> Result<()> {
        Ok(())
    }
    fn create_shader_id(
        &mut self, _id: IdentifierIdx, _pso: IdentifierIdx, _dir: &Directive,
    ) -> Result<()> {
        Ok(())
    }
    fn create_shader_table(
        &mut self, _id: IdentifierIdx, _pso: IdentifierIdx, _root_vals: &[RootVal],
        _shader_ids: &[Option<IdentifierIdx>], _dir: &Directive,
    ) -> Result<()> {
        Ok(())
    }
    fn create_compute_pipeline(
        &mut self, _id: IdentifierIdx, _shaders: IdentifierIdx, _root: Option<IdentifierIdx>,
        _dir: &Directive,
    ) -> Result<()> {
        Ok(())
    }
    fn create_pipeline_state_object(
        &mut self, _id: IdentifierIdx, _add_to: Option<IdentifierIdx>, _libs: &[IdentifierIdx],
        _collections: &[IdentifierIdx], _dir: &Directive,
    ) -> Result<()> {
        Ok(())
    }
    fn create_view(
        &mut self, _id: IdentifierIdx, _buffer: Option<IdentifierIdx>, _dir: &Directive,
    ) -> Result<()> {
        Ok(())
    }
    fn create_command_signature(
        &mut self, _id: IdentifierIdx, _root_sig: Option<IdentifierIdx>, _dir: &Directive,
    ) -> Result<()> {
        Ok(())
    }
    fn dispatch(
        &mut self, _pipeline: IdentifierIdx, _ids: &RootVal, _content: DispatchContent,
        _root_sig: Option<IdentifierIdx>, _dir: &Directive,
    ) -> Result<Duration> {
        Ok(Default::default())
    }

    /// Get the byte representation of the shader identifier
    fn get_shader_id(&mut self, _id: IdentifierIdx) -> Result<Vec<u8>> { Ok(Default::default()) }

    fn get_gpuva(&self, _buffer: IdentifierIdx, _typ: IdentifierType) -> Result<u64> {
        Ok(Default::default())
    }

    /// Upload data to a GPU buffer
    fn upload(&mut self, _buffer: IdentifierIdx, _f: &mut dyn FnMut(&mut [u8])) -> Result<()> {
        Ok(())
    }

    /// Download data from a GPU buffer
    fn download(&mut self, _buffer: IdentifierIdx) -> Result<Vec<u8>> { Ok(Vec::new()) }
}

pub(crate) fn create(ty: BackendType, args: &crate::Args) -> Result<Box<dyn Backend>> {
    match ty {
        #[cfg(feature = "enable_dx12")]
        BackendType::Dx12 => Ok(Box::new(dx12::Dx12Backend::new(args)?)),
        BackendType::Null => Ok(Box::new(null::NullBackend::new(args)?)),
    }
}

pub(crate) fn create_with_window<F: FnMut(&mut dyn Backend) -> Continue + 'static>(
    ty: BackendType, args: &crate::Args, render_callback: F,
) -> Result<Box<dyn Window>> {
    let render_callback = Box::new(render_callback);
    match ty {
        #[cfg(feature = "enable_dx12")]
        BackendType::Dx12 => Ok(dx12::Dx12Backend::with_window(args, render_callback)?),
        BackendType::Null => Ok(null::NullBackend::with_window(args, render_callback)?),
    }
}

pub(crate) fn devices(ty: BackendType) -> Result<Vec<String>> {
    match ty {
        #[cfg(feature = "enable_dx12")]
        BackendType::Dx12 => Ok(dx12::Dx12Backend::devices()?),
        BackendType::Null => Ok(null::NullBackend::devices()?),
    }
}
