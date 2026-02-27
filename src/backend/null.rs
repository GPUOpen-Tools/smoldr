// Copyright (c) Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

use miette::{Result, bail};
use tracing::{info, trace, warn};

use crate::backend::{Backend, Continue, RenderCallback, Window};
use crate::{Directive, IdentifierIdx, ResultExt};

#[derive(Default)]
pub(crate) struct NullBackend {
    buffers: HashMap<IdentifierIdx, Vec<u8>>,
}

pub(crate) struct NullWindow {
    backend: NullBackend,
    render_callback: RenderCallback,
}

impl Window for NullWindow {
    fn main_loop(&mut self) -> Result<()> {
        loop {
            if (self.render_callback)(&mut self.backend) == Continue::Exit {
                break Ok(());
            }
        }
    }
}

impl Backend for NullBackend {
    fn new(_: &crate::Args) -> Result<Self> { Ok(Default::default()) }
    fn with_window(args: &crate::Args, render_callback: RenderCallback) -> Result<Box<dyn Window>> {
        Ok(Box::new(NullWindow { backend: Self::new(args)?, render_callback }))
    }

    fn reset(&mut self) { self.buffers.clear(); }

    fn compile(
        &mut self, _: IdentifierIdx, source: &str, path: &Path, dir: &Directive,
    ) -> Result<()> {
        let Directive::Object { shader_model, entrypoint, args, .. } = dir else { unreachable!() };

        let infile = tempfile::Builder::new()
            .suffix("smoldr.hlsl")
            .tempfile()
            .context("Creating temporary file for source code")?;

        let outfile = tempfile::Builder::new()
            .suffix("smoldr.dx")
            .tempfile()
            .context("Creating temporary file for compiled code")?;

        let mut cmd = Command::new("dxc");
        cmd.args(["-T", shader_model, "-E", entrypoint.as_deref().unwrap_or_default()]);

        if let Some(p) = path.parent() {
            cmd.arg("-I");
            cmd.arg(p);
        }

        cmd.arg("-Fo")
            .args([outfile.path().as_os_str(), infile.path().as_os_str()])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        std::fs::write(infile.path(), source).context("Failed to write input file")?;

        trace!("Starting dxc {:?}", cmd.get_args());

        let child = cmd.spawn().context("Starting dxc")?;

        let res = child.wait_with_output().context("Failed to read stdout")?;

        if !res.stdout.is_empty() {
            info!("dxc stdout:\n{}", String::from_utf8_lossy(&res.stdout).trim());
        }

        if !res.stderr.is_empty() {
            warn!("dxc stderr:\n{}", String::from_utf8_lossy(&res.stderr).trim());
        }

        if !res.status.success() {
            bail!("dxc exited with errors");
        }

        let _output = std::fs::read(outfile.path()).context("Failed to read output files")?;
        Ok(())
    }

    fn create_buffer(&mut self, id: IdentifierIdx, dir: &Directive) -> Result<()> {
        let Directive::Buffer { content, .. } = dir else { unreachable!() };

        self.buffers.insert(id, vec![0; content.len()]);
        Ok(())
    }

    fn upload(&mut self, buffer_id: IdentifierIdx, f: &mut dyn FnMut(&mut [u8])) -> Result<()> {
        f(&mut *self.buffers.get_mut(&buffer_id).unwrap());
        Ok(())
    }

    fn download(&mut self, buffer_id: IdentifierIdx) -> Result<Vec<u8>> {
        Ok(self.buffers[&buffer_id].clone())
    }
}
