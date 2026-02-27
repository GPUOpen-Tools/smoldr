// Copyright (c) Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for user-facing errors generated when running a file.
use std::io;
use std::path::Path;

use miette::Result;
use regex::Regex;
use test_log::test;

use super::get_error;
use crate::{Directive, State, backend};

fn parse(state: &mut State, input: &str) -> Result<Vec<Directive>> {
    let base_path = Path::new("src").join("tests").join("run.sm");
    state.parse_stream("input".to_string(), &base_path, &mut io::Cursor::new(input))
}

fn run_test(input: &str) -> String {
    let mut state = State::default();
    let statements = parse(&mut state, input).expect("Failed to parse");
    match run(&mut state, &statements) {
        Ok(()) => String::new(),
        r => {
            let err = get_error(r);
            // Remove everything between / and / or Windows style paths to get the same output on
            // Windows and Linux.
            let paths = Regex::new(r"(?s)(src)?/.*/|[A-Z]:\\.*(\\|/)").unwrap();
            paths.replace_all(&err, "").into_owned()
        }
    }
}

/// Check tests in run.sm.
///
/// run.sm is in the same same format as tree-sitter tests.
/// A test name, then the test input and the expected output.
/// The expected output must be empty for a passing test or match the error message.
/// The error message is slightly processed to eliminate differences between operating systems.
#[test]
fn test() -> std::io::Result<()> {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("tests").join("run.sm");
    let expected_file = Path::new(env!("OUT_DIR")).join("run.sm");

    crate::tests::run_test_file(&file, &expected_file, &mut run_test)
}

fn run(state: &mut State, directives: &[Directive]) -> Result<()> {
    let mut backend = backend::create(backend::BackendType::Null, &state.args)?;
    state.apply_directives(&mut *backend, directives)
}
