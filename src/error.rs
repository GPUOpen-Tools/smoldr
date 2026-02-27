// Copyright (c) Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

// Ignore unused warnings, the fields are used in the error formatting.
#![allow(unused_assignments)]

use std::ops::Range;

use miette::Diagnostic;
use std::fmt::Write;
use thiserror::Error;

use crate::parser::{Identifier, SpanObj};
use crate::{DataType, IdentifierType, Value, Values};

fn comma_join<'a, T: ToString + 'a, I: IntoIterator<Item = &'a T>>(iter: I) -> String {
    iter.into_iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
}

fn comma_join_lowercase<'a, I: IntoIterator<Item = &'a &'static str>>(iter: I) -> String {
    iter.into_iter().map(|s| s.to_lowercase()).collect::<Vec<_>>().join(", ")
}

/// Create the help string that is used with Expect and ExpectEpsilon errors
fn create_expect_help(
    resource_name: &Identifier, offset: usize, expected: &Values, buffer_content: &Values,
) -> String {
    let (a, b) = aligned_buffer_format(expected, buffer_content);
    // Keep description short to avoid word wrap as much as possible
    let prefix_a = format!("Expected at OFFSET {offset}:");
    let prefix_b = format!("Differences in {resource_name}:");
    let width = prefix_a.len().max(prefix_b.len());
    format!("  ╰{}\n{prefix_a:>width$}{a}\n{prefix_b:>width$}{b}\n", "─".repeat(60))
}

/// Formats two number sequences to be aligned. The expected will always print all numbers,
/// the actually observed sequence only prints the numbers that are incorrect
/// Example:   [0, 13, 8, 114] and [0, 13, 8, 6]
/// will result in " 0 13 8 114"
///                "    1     6"
fn aligned_buffer_format(expected: &Values, actual: &Values) -> (String, String) {
    let mut expect_out = String::new();
    let mut actual_out = String::new();

    for (e, a) in expected.data.iter().zip(actual.data.iter()) {
        let width = e.to_string().len().max(a.to_string().len());
        write!(expect_out, " {e:>width$}").unwrap();
        if e != a {
            // Write diff
            write!(actual_out, " {a:>width$}").unwrap();
        } else {
            // values identical -> just write spaces
            write!(actual_out, " {:>width$}", ' ').unwrap();
        }
    }
    (expect_out, actual_out)
}

#[derive(Diagnostic, Debug, Error)]
pub(crate) enum ParserError {
    #[error("Can only have a single {arg0}")]
    #[diagnostic(code(smoldr::parser::DuplicateArgument), help("Remove one of the {arg0}s"))]
    DuplicateArgument {
        #[label = "First {arg0} declared here"]
        arg0: Identifier,
        #[label = "Second {arg1} declared here"]
        arg1: Identifier,
    },
    #[error("Invalid character hex code")]
    #[diagnostic(code(smoldr::parser::InvalidCharCode))]
    InvalidCharCode {
        #[label = "Not a valid character"]
        span: Range<usize>,
    },
    #[error("Unknown character escape")]
    #[diagnostic(
        code(smoldr::parser::UnknownCharacterEscape),
        help(
            "Allowed escapes are \\, \" and two hex digits for a character code.\nIf you meant to \
             write a literal backslash, use \\\\."
        )
    )]
    UnknownCharacterEscape {
        #[label = "Unknown character escape"]
        span: Range<usize>,
    },
    #[error("Invalid flag")]
    #[diagnostic(code(smoldr::parser::InvalidFlag))]
    InvalidFlag {
        #[label("Must be one of {}", comma_join_lowercase(flags))]
        span: Range<usize>,
        flags: Vec<&'static str>,
    },
    #[error("Invalid integer")]
    #[diagnostic(code(smoldr::parser::InvalidInteger))]
    InvalidInteger {
        #[label("Expected type {typ} here")]
        span: Range<usize>,
        typ: String,
    },
    #[error("Invalid {} statement", dir_format.split_once(' ').map(|r| r.0).unwrap_or(dir_format))]
    #[diagnostic(code(smoldr::parser::InvalidStatement), help("Expected format: {dir_format}"))]
    InvalidStatement {
        #[label("Declared here")]
        decl: Range<usize>,
        #[label("Invalid here")]
        span: Range<usize>,
        dir_format: &'static str,
    },
    #[error("Invalid {name}")]
    #[diagnostic(code(smoldr::parser::InvalidWord))]
    InvalidWord {
        #[label("Must be one of {}", comma_join(expected.iter()))]
        span: Range<usize>,
        name: &'static str,
        expected: &'static [&'static str],
    },
    #[error("Failed to parse time")]
    #[diagnostic(
        code(smoldr::parser::ParseDuration),
        help("Durations must be of the form `(<num><unit> )+` like `5s 200ms`")
    )]
    ParseDuration {
        source: humantime::DurationError,
        #[label("Expected duration here")]
        duration_span: Range<usize>,
        #[label("As part of this statement")]
        identifier: Identifier,
    },
    #[error("Data inconsistent with size")]
    #[diagnostic(code(smoldr::parser::RawSizeMismatch))]
    RawSizeMismatch {
        #[label = "Data size specified as {size}"]
        size: SpanObj<u64>,
        #[label = "Content has size {content_size}"]
        content: Range<usize>,
        content_size: usize,
    },
    #[error("Undelimited statement, `END` not found")]
    #[diagnostic(
        code(smoldr::parser::UndelimitedStatement),
        help("Statement must end with an `END` on a single line")
    )]
    UndelimitedStatement {
        #[label = "Statement starts here"]
        span: Range<usize>,
    },
    #[error(
        "Unknown parser error, you should not see this, please report a bug with the content of \
         the file"
    )]
    #[diagnostic(code(smoldr::parser::Unknown))]
    Unknown {
        #[label = "Unknown statement starts here"]
        span: Range<usize>,
    },
    #[error("Unknown statement")]
    #[diagnostic(
        code(smoldr::parser::UnknownStatement),
        help("Try one of the statements from the documentation")
    )]
    UnknownStatement {
        #[label = "Statement starts here"]
        span: Range<usize>,
    },
}

impl<I: winnow::stream::Stream + winnow::stream::Location> winnow::error::ParserError<I>
    for ParserError
{
    type Inner = Self;

    fn from_input(input: &I) -> Self {
        let mut pos = input.current_token_start();
        if input.eof_offset() == 0 {
            pos = pos.saturating_sub(1);
        }
        Self::Unknown { span: pos..pos }
    }
    fn into_inner(self) -> Result<Self::Inner, Self> { Ok(self) }

    /// Return `self` except if it is unknown, then return `other`.
    fn or(self, other: Self) -> Self {
        if !matches!(self, ParserError::Unknown { .. }) { other } else { self }
    }
}

// Runtime errors

#[derive(Diagnostic, Debug, Error)]
#[error("An identifier with this name already exists")]
#[diagnostic(code(smoldr::DuplicateIdentifier), help("Choose another name for one of the objects"))]
pub(crate) struct DuplicateIdentifier {
    #[label("{name0} was first defined here")]
    pub(crate) name0: Identifier,
    #[label = "and then defined again here"]
    pub(crate) name1: Identifier,
}

#[derive(Diagnostic, Debug, Error)]
#[error("'NULL' is a keyword and not a valid identifier")]
#[diagnostic(code(smoldr::NullIdentifier), help("Choose a different name the object"))]
pub(crate) struct NullIdentifier {
    #[label("{name} is defined here")]
    pub(crate) name: Identifier,
}

#[derive(Diagnostic, Debug, Error)]
#[error("A view must have a buffer, except if it is a raytracing acceleration structure")]
#[diagnostic(code(smoldr::ViewNoBuffer), help("Specify a buffer for the view"))]
pub(crate) struct ViewNoBuffer {
    #[label("{name} is defined here")]
    pub(crate) name: Identifier,
}

#[derive(Diagnostic, Debug, Error)]
#[error("Buffer content does not match expected data")]
#[diagnostic(
    code(smoldr::Expect),
    help("{}", create_expect_help(buffer, *offset, expected, buffer_content))
)]
pub(crate) struct Expect {
    #[label("{buffer} contains {buffer_first} here")]
    pub(crate) first: Identifier,
    pub(crate) buffer: Identifier,
    pub(crate) buffer_first: Value,
    pub(crate) buffer_content: Values,
    pub(crate) expected: Values,
    pub(crate) offset: usize,
}

#[derive(Diagnostic, Debug, Error)]
#[error("Buffer content does not match expected data within epsilon {epsilon}")]
#[diagnostic(
    code(smoldr::ExpectEpsilon),
    help("{}", create_expect_help(buffer, *offset, expected, buffer_content))
)]
pub(crate) struct ExpectEpsilon {
    #[label("{buffer} contains {buffer_first} here")]
    pub(crate) first: Identifier,
    pub(crate) buffer: Identifier,
    pub(crate) buffer_first: Value,
    pub(crate) buffer_content: Values,
    pub(crate) expected: Values,
    pub(crate) offset: usize,
    pub(crate) epsilon: Value,
}

#[derive(Diagnostic, Debug, Error)]
#[error("Expected data check range {offset}..{} is out of bounds", offset.content + expect_size)]
#[diagnostic(code(smoldr::ExpectOutOfBounds))]
pub(crate) struct ExpectOutOfBounds {
    #[label("Checking buffer declared with byte size {buffer_size}")]
    pub(crate) buffer: Identifier,
    pub(crate) buffer_size: usize,
    #[label("Offset declared here")]
    pub(crate) offset: SpanObj<usize>,
    pub(crate) expect_size: usize,
}

#[derive(Diagnostic, Debug, Error)]
#[error("Shader identifier assertion failed")]
#[diagnostic(code(smoldr::AssertShaderId), help("Shader identifiers are expected to {} but do not",
    if *equal { "match" } else { "be different" }))]
pub(crate) struct AssertShaderId {
    #[label("{id_a} is {content_a}")]
    pub(crate) id_a: Identifier,
    #[label("{id_b} is {content_b}")]
    pub(crate) id_b: Identifier,
    pub(crate) content_a: Values,
    pub(crate) content_b: Values,
    pub(crate) equal: bool,
}

#[derive(Diagnostic, Debug, Error)]
#[error("Compute pipeline must have exactly 1 shader but has {shader_count}")]
#[diagnostic(code(smoldr::InvalidShaderCount))]
pub(crate) struct InvalidShaderCount {
    #[label("Pipeline defined here")]
    pub(crate) name: Identifier,
    pub(crate) shader_count: usize,
}

#[derive(Diagnostic, Debug, Error)]
#[error("Cannot get shader identifier for {:?}", name.content)]
#[diagnostic(code(smoldr::NullShaderId))]
#[allow(dead_code)]
pub(crate) struct NullShaderId {
    #[label("{name} is queried here")]
    pub(crate) name: Identifier,
}

#[derive(Diagnostic, Debug, Error)]
#[error("Shader record with {stride} bytes is larger than the maximum {max_size} bytes")]
#[diagnostic(code(smoldr::TooManyLocalRootEntries))]
#[allow(dead_code)]
pub(crate) struct TooManyLocalRootEntries {
    #[label("In the shader table defined here")]
    pub(crate) name: Identifier,
    pub(crate) stride: usize,
    pub(crate) max_size: usize,
}

#[derive(Diagnostic, Debug, Error)]
#[error("No object called '{name}' found")]
#[diagnostic(code(smoldr::UnknownIdentifier))]
pub(crate) struct UnknownIdentifier {
    #[label("{name} is used here")]
    pub(crate) name: Identifier,
}

#[derive(Diagnostic, Debug, Error)]
#[error("Expected one of {} but got {actual}", comma_join(expected))]
#[diagnostic(code(smoldr::WrongType))]
pub(crate) struct WrongType {
    pub(crate) expected: Vec<IdentifierType>,
    pub(crate) actual: IdentifierType,
    #[label("{declaration} is declared here as {actual}")]
    pub(crate) declaration: Identifier,
    #[label("and used here, where one of {} is expected", comma_join(expected))]
    pub(crate) used: Identifier,
}

#[derive(Diagnostic, Debug, Error)]
#[error("Failed to run statement")]
#[diagnostic(
    code(smoldr::ApplyDirective),
    help("Using `--validate` sometimes provides better errors")
)]
pub(crate) struct ApplyDirective {
    #[diagnostic_source]
    pub(crate) cause: miette::Report,
    #[label = "defined here"]
    pub(crate) declaration: Identifier,
}

// Same as ApplyDirective but without the help message
#[derive(Diagnostic, Debug, Error)]
#[error("Failed to run statement")]
#[diagnostic(code(smoldr::ApplyDirectiveValidation))]
pub(crate) struct ApplyDirectiveValidation {
    #[diagnostic_source]
    pub(crate) cause: miette::Report,
    #[label = "defined here"]
    pub(crate) declaration: Identifier,
}

#[derive(Diagnostic, Debug, Error)]
#[error("Invalid data type")]
#[diagnostic(code(smoldr::InvalidDataType))]
#[allow(dead_code)]
pub(crate) struct InvalidDataType {
    #[label = "Type {typ} cannot be used in here"]
    pub(crate) declaration: Identifier,
    pub(crate) typ: DataType,
}

#[derive(Diagnostic, Debug, Error)]
#[error("Buffer is not large enough to hold ExecuteIndirect arguments: Needs >= {} bytes but buffer has only {} bytes after offset {offset}", u64::from(*stride) * u64::from(*max_commands), buffer_size - offset)]
#[diagnostic(code(smoldr::TooSmallExecuteIndirectBuffer))]
#[allow(dead_code)]
pub(crate) struct TooSmallExecuteIndirectBuffer {
    #[label("Argument buffer with size {buffer_size} used here")]
    pub(crate) buffer: Identifier,
    #[label("ExecuteIndirect specifies offset {offset} and {max_commands} max commands")]
    pub(crate) execute_indirect: Identifier,
    #[label("Signature specifies stride {stride}")]
    pub(crate) signature: Identifier,
    pub(crate) buffer_size: u64,
    pub(crate) offset: u64,
    pub(crate) max_commands: u32,
    pub(crate) stride: u32,
}

#[derive(Diagnostic, Debug, Error)]
#[error("Stride too small, must be at least {need_stride} bytes for the declared signature")]
#[diagnostic(code(smoldr::TooSmallExecuteIndirectStride))]
#[allow(dead_code)]
pub(crate) struct TooSmallExecuteIndirectStride {
    #[label("Signature with stride {stride} declared here")]
    pub(crate) declaration: Identifier,
    pub(crate) stride: u32,
    pub(crate) need_stride: usize,
}

#[derive(Diagnostic, Debug, Error)]
#[error("Incorrect results in {} EXPECT statements", errors.len())]
#[diagnostic(code(smoldr::Failure))]
#[allow(dead_code)]
pub(crate) struct Failure {
    #[related]
    pub(crate) errors: Vec<miette::Report>,
}

#[derive(Diagnostic, Debug, Error)]
#[error("Aborting due to too many ({}) failed EXPECT statements", errors.len())]
#[diagnostic(code(smoldr::Abort))]
#[allow(dead_code)]
pub(crate) struct Abort {
    #[related]
    pub(crate) errors: Vec<miette::Report>,
}
