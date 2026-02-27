// Copyright (c) Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::Path;

use miette::Report;

use super::parse_str;

/// Parses the input and returns the debug formatting of the parsed statements.
fn parse_input(input: &str) -> String {
    match parse_str(input) {
        Ok(output) => format!("{output:#?}"),
        Err(e) => crate::tests::error_to_str(Report::new(e).with_source_code(input.to_string())),
    }
}

/// Check tests in statements.sm.
///
/// statements.sm is in the same same format as tree-sitter tests.
/// A test name, then the test input and the expected output.
/// The expected output must match the debug formatting of the parsed statements.
#[test]
fn test() -> std::io::Result<()> {
    let file =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("parser").join("statements.sm");
    let expected_file = Path::new(env!("OUT_DIR")).join("statements.sm");

    crate::tests::run_test_file(&file, &expected_file, &mut parse_input)
}
