// Copyright (c) Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::num::{NonZero, NonZeroU64};
use std::ops::{Range, RangeInclusive};
use std::{fmt, mem};

use half::f16;
use memchr::memchr_iter;
use miette::Report;
use num_traits::Num;
use winnow::Parser as _;
use winnow::ascii::{
    alpha1, alphanumeric1, digit0, digit1, escaped, hex_digit1, line_ending, space1,
    till_line_ending,
};
use winnow::combinator::{
    alt, cut_err, delimited, dispatch, empty, eof, fail, opt, peek, preceded, repeat, separated,
    seq, terminated,
};
use winnow::error::ErrMode;
use winnow::stream::{AsChar, Location, Stream};
use winnow::token::{literal, none_of, one_of, rest, take, take_till, take_until, take_while};
use winnow::{LocatingSlice, Stateful};

use crate::error::ParserError;
use crate::{
    Aabb, Bind, CommandSignatureArgument, DataType, Dim3, Directive, DispatchType, DumpDataType,
    DumpFormat, Export, Fill, HitGroup, InputViewType, LinkObject, PipelineStateObjectType,
    PipelineType, ProceduralGeometry, RootSigConst, RootSigEntry, RootSigTable, RootSigView,
    RootValView, ShaderReference, SourceFileIdx, TlasBlas, Transform, TriangleGeometry,
    UnresolvedRootVal, UnresolvedRootValConst, UnresolvedShaderTableRecord, UnresolvedValue,
    UnresolvedValueContent, UnresolvedValues, Value, ValueContent, Vertex, ViewType,
};

#[cfg(test)]
mod tests;

/// Same as `dispatch!` but doesn't move captured variables into the closure.
macro_rules! dispatch_no_move {
    (
        $scrutinee_parser:expr;
        $( $arm_pat:pat $(if $arm_pred:expr)? => $arm_parser: expr ),+ $(,)?
    ) => {
        |i: &mut _|
        {
            let initial = $scrutinee_parser.parse_next(i)?;
            match initial {
                $(
                    $arm_pat $(if $arm_pred)? => $arm_parser.parse_next(i),
                )*
            }
        }
    }
}

/// A `dispatch_no_move!` that parses an `identifier` as `id` and matches on the string.
macro_rules! dispatch_id {
    (
        $id:ident;
        $( $arm_pat:pat $(if $arm_pred:expr)? => $arm_parser: expr ),+ $(,)?
    ) => {
        |i: &mut _|
        {
            let $id = identifier.parse_next(i)?;
            match $id.content.as_str() {
                $(
                    $arm_pat $(if $arm_pred)? => $arm_parser.parse_next(i),
                )*
            }
        }
    }
}

type Error = ErrMode<ParserError>;
type Result<T, E = Error> = std::result::Result<T, E>;
type Input<'a> = Stateful<LocatingSlice<&'a str>, ParserState>;

trait Parser<'a, Output>: winnow::Parser<Input<'a>, Output, Error> {}
impl<'a, Output, T: winnow::Parser<Input<'a>, Output, Error>> Parser<'a, Output> for T {}

#[derive(Debug, Default)]
struct ParserState {
    source_file: SourceFileIdx,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DispatchParseType {
    Dispatch,
    DispatchRays,
    ExecuteIndirect,
}

pub(crate) type Identifier = SpanObj<String>;

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) struct SpanObj<T> {
    pub(crate) content: T,
    pub(crate) source_file: SourceFileIdx,
    pub(crate) span: Range<usize>,
}

impl<T> SpanObj<T> {
    pub(crate) fn map_content<U>(&self, u: U) -> SpanObj<U> {
        SpanObj { content: u, source_file: self.source_file, span: self.span.clone() }
    }
}

impl<T> From<SpanObj<T>> for miette::SourceSpan {
    fn from(i: SpanObj<T>) -> Self { i.span.into() }
}

impl<T: fmt::Display> fmt::Display for SpanObj<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.content)?;
        Ok(())
    }
}

pub(crate) fn parse(
    input: &str, state: &crate::State, source_file: SourceFileIdx,
) -> Result<Vec<Directive>, Report> {
    let parser_state = ParserState { source_file };
    let input = Stateful { input: LocatingSlice::new(input), state: parser_state };
    statements.parse(input).map_err(|e| {
        Report::new(e.into_inner()).with_source_code(state.get_named_source(source_file))
    })
}

#[cfg(test)]
pub(crate) fn parse_str(input: &str) -> Result<Vec<Directive>, ParserError> {
    let input = Stateful { input: LocatingSlice::new(input), state: Default::default() };
    statements.parse(input).map_err(winnow::error::ParseError::into_inner)
}

fn span_stream<T>((t, span): (T, Range<usize>)) -> Stateful<T, Range<usize>> {
    Stateful { input: t, state: span }
}

/// From `#` or `//` to end of line.
fn singleline_comment(s: &mut Input) -> Result<()> {
    alt(("//", "#")).parse_next(s)?;
    till_line_ending(s)?;
    Ok(())
}

/// From `/*` to `*/`, can be nested.
fn multiline_comment(s: &mut Input) -> Result<()> {
    "/*".parse_next(s)?;
    loop {
        take_till(0.., ('/', '*')).parse_next(s)?;
        if peek::<_, _, (), _>("/*").parse_next(s).is_ok() {
            // Nested
            multiline_comment(s)?;
        } else if literal::<_, _, ()>("*/").parse_next(s).is_ok() {
            break Ok(());
        } else {
            // Neither /* nor */, continue parsing
            take(1usize).parse_next(s)?;
        }
    }
}

fn none<T>(s: &mut Input) -> Result<Option<T>> {
    '-'.parse_next(s)?;
    Ok(None)
}

/// whitespace or multiline comments (everything that can occur at any place).
fn space(s: &mut Input) -> Result<()> {
    repeat::<_, _, (), _, _>(1.., alt((space1.map(|_| ()), multiline_comment))).parse_next(s)?;
    Ok(())
}

/// Single line ending, optionally with a comment or space.
fn single_line_end(s: &mut Input) -> Result<()> {
    (opt(space), opt(singleline_comment)).parse_next(s)?;
    if eof::<_, ()>(s).is_ok() {
        return Ok(());
    }
    line_ending(s)?;
    Ok(())
}

/// One or more line endings, optionally with a comment or space.
fn line_end(s: &mut Input) -> Result<()> {
    single_line_end(s)?;
    repeat::<_, _, (), _, _>(0.., alt((space, singleline_comment, line_ending.void())))
        .parse_next(s)?;
    Ok(())
}

/// Identifier, like a variable. Contains only a-zA-Z0-9_ though digits not as the first char.
fn word<'a>(s: &mut Input<'a>) -> Result<&'a str> {
    (alt((alpha1, "_")), opt(repeat::<_, _, (), _, _>(0.., alt((alphanumeric1, "_")))))
        .take()
        .parse_next(s)
}

/// Parse and return with span.
fn span_obj<'a, Output, ParseNext>(
    mut parser: ParseNext,
) -> impl Parser<'a, SpanObj<Output>> + use<'a, Output, ParseNext>
where ParseNext: Parser<'a, Output> {
    move |s: &mut Input<'a>| {
        let res = parser.by_ref().with_span().parse_next(s)?;

        Ok(SpanObj { content: res.0, source_file: s.state.source_file, span: res.1 })
    }
}

/// Identifier, like a variable. Contains only a-zA-Z0-9_ though digits not as the first char.
fn identifier(s: &mut Input) -> Result<Identifier> { span_obj(word.output_into()).parse_next(s) }

/// Unsigned natural number.
fn uint<T: Num>(s: &mut Input) -> Result<T> {
    let from_radix = |radix| {
        move |s: &mut Stateful<&'_ str, Range<usize>>| {
            T::from_str_radix(s.input, radix).map_err(|_| {
                ErrMode::Backtrack(ParserError::InvalidInteger {
                    span: s.state.clone(),
                    typ: std::any::type_name::<T>().into(),
                })
            })
        }
    };

    (alt((
        preceded("0x", cut_err(hex_digit1.with_span().map(span_stream).and_then(from_radix(16)))),
        preceded(
            "0b",
            cut_err(
                repeat::<_, _, (), _, _>(1.., one_of(['0', '1']))
                    .take()
                    .with_span()
                    .map(span_stream)
                    .and_then(from_radix(2)),
            ),
        ),
        digit1.with_span().map(span_stream).and_then(from_radix(10)),
    )))
    .parse_next(s)
}

fn non_zero_u64(s: &mut Input) -> Result<NonZeroU64> {
    let i = uint(s)?;
    if let Some(i) = NonZero::new(i) { Ok(i) } else { fail(s) }
}

/// Any number, also allows floating point.
fn number<T: Num + std::str::FromStr + std::ops::Neg<Output = T>>(s: &mut Input) -> Result<T> {
    let start = s.checkpoint();
    let neg = opt(one_of(['+', '-'])).parse_next(s)? == Some('-');

    match uint::<T>(s) {
        Ok(mut r) => {
            if s.starts_with(['.', 'E', 'e']) {
                // Actually a float
                s.reset(&start);
            } else {
                if neg {
                    r = -r;
                }
                return Ok(r);
            }
        }
        Err(_) => s.reset(&start),
    }

    // Parse float
    (
        opt(one_of(['+', '-'])),
        alt((
            // At least one digit before or after the dot
            preceded('.', digit1).void(),
            (digit1, '.', digit0).void(),
        )),
        opt((one_of(['E', 'e']), opt(one_of(['+', '-'])), digit1)),
    )
        .take()
        .parse_to()
        .parse_next(s)
}

fn value<'a>(typ: DataType) -> impl Parser<'a, Value> {
    move |s: &mut Input<'a>| {
        Ok(match typ {
            DataType::U64 => Value::U64(uint::<u64>(s)?),
            DataType::U32 => Value::U32(uint::<u32>(s)?),
            DataType::U16 => Value::U16(uint::<u16>(s)?),
            DataType::U8 => Value::U8(uint::<u8>(s)?),
            DataType::F32 => Value::F32(number::<f32>(s)?),
            DataType::F16 => Value::F16(number::<f16>(s)?),
        })
    }
}

#[allow(clippy::type_complexity)]
const STRING_UNQUOTED_CHARS_NO_EQUALS: (
    fn(char) -> bool,
    char,
    char,
    char,
    char,
    RangeInclusive<char>,
    RangeInclusive<char>,
    char,
    char,
    char,
) = (<char as AsChar>::is_alpha, '!', '$', '%', '&', '*'..='<', '>'..='@', '^', '_', '~');

#[allow(clippy::type_complexity)]
const STRING_UNQUOTED_CHARS: (
    (
        fn(char) -> bool,
        char,
        char,
        char,
        char,
        RangeInclusive<char>,
        RangeInclusive<char>,
        char,
        char,
        char,
    ),
    char,
) = (STRING_UNQUOTED_CHARS_NO_EQUALS, '=');

fn string_escapes<'a, Normal: Parser<'a, char>>(mut normal: Normal) -> impl Parser<'a, String> {
    move |s: &mut Input<'a>| {
        let r: String = escaped(
            normal.by_ref(),
            '\\',
            alt((
                '\\',
                '"',
                take_while(2, AsChar::is_hex_digit).with_span().map(span_stream).and_then(
                    |s: &mut Stateful<&'_ str, Range<usize>>| {
                        char::from_u32(
                            u32::from_str_radix(s.input, 16).expect("Failed parsing hex escape"),
                        )
                        .ok_or_else(|| {
                            ErrMode::Cut(ParserError::InvalidCharCode { span: s.state.clone() })
                        })
                    },
                ),
                |s: &mut Input| {
                    let span = s.current_token_start()..s.current_token_start() + 1;
                    fail.map_err(|()| {
                        ErrMode::Cut(ParserError::UnknownCharacterEscape { span: span.clone() })
                    })
                    .parse_next(s)
                },
            )),
        )
        .parse_next(s)?;

        if r.is_empty() { fail.parse_next(s) } else { Ok(r) }
    }
}

/// Quoted strings use llvm-like syntax, allowing a backslash followed by two hex digits like `\0a`.
fn quoted_string(s: &mut Input) -> Result<String> {
    delimited('"', string_escapes(none_of(['\\', '"'])), '"').parse_next(s)
}

/// Unquoted or quoted string.
fn string(s: &mut Input) -> Result<String> {
    if s.starts_with('"') {
        quoted_string(s)
    } else {
        let res = string_escapes(one_of(STRING_UNQUOTED_CHARS)).parse_next(s)?;
        if res.is_empty() {
            return fail.parse_next(s);
        }
        Ok(res)
    }
}

/// Unquoted string without equals sign or quoted string.
fn string_no_equals(s: &mut Input) -> Result<String> {
    if s.starts_with('"') {
        quoted_string(s)
    } else {
        string_escapes(one_of(STRING_UNQUOTED_CHARS_NO_EQUALS)).parse_next(s)
    }
}

fn parse_flag<F: bitflags::Flags + Clone + fmt::Debug>(
    s: &mut Stateful<&'_ str, Range<usize>>,
) -> Result<F> {
    // Check that the string is lowercase
    if s.input == s.input.to_lowercase() {
        if let Some(r) = F::from_name(&s.input.to_uppercase()) {
            return Ok(r);
        }
    }

    Err(ErrMode::Backtrack(ParserError::InvalidFlag {
        span: s.state.clone(),
        flags: F::FLAGS.iter().map(|f| f.name()).collect::<Vec<_>>(),
    }))
}

/// Parse a CONFIG into bitflags.
fn flags<'a, F: bitflags::Flags + Clone + fmt::Debug + Default>(
    config: &mut (Option<F>, Option<Identifier>), id: Identifier,
) -> impl Parser<'a, ()> + use<'a, '_, F> {
    no_dup(
        config,
        id,
        terminated(
            repeat(
                1..,
                preceded(space, word).with_span().map(span_stream).and_then(parse_flag::<F>),
            )
            .fold(F::empty, bitflags::Flags::union),
            line_end,
        ),
    )
}

/// Parser with state that fails if the state is already filled, otherwise parses into the state.
fn no_dup<'a, 'b, Output, ParseNext>(
    opt: &'b mut (Option<Output>, Option<Identifier>), mut id: Identifier, mut parser: ParseNext,
) -> impl Parser<'a, ()> + use<'a, 'b, Output, ParseNext>
where ParseNext: Parser<'a, Output> {
    move |s: &mut Input<'a>| {
        if let Some(old) = &opt.1 {
            return Err(ErrMode::Backtrack(ParserError::DuplicateArgument {
                arg0: old.clone(),
                arg1: id.clone(),
            }));
        }
        let output = parser.parse_next(s)?;
        opt.0 = Some(output);
        opt.1 = Some(mem::take(&mut id));
        Ok(())
    }
}

fn data_type(s: &mut Input) -> Result<DataType> {
    dispatch_id! {id;
        "uint64" => empty.value(DataType::U64),
        "uint32" => empty.value(DataType::U32),
        "uint16" => empty.value(DataType::U16),
        "uint8" => empty.value(DataType::U8),
        "float" => empty.value(DataType::F32),
        "float16" => empty.value(DataType::F16),
        _ => inv_word(id.span, "type", &["uint64", "uint32", "uint16", "uint8", "float", "float16",
            "float16", "float16", "float16"]),
    }
    .parse_next(s)
}

fn view_type(s: &mut Input) -> Result<ViewType> {
    dispatch_id! {id;
        "SRV" => empty.value(ViewType::Srv),
        "UAV" => empty.value(ViewType::Uav),
        _ => inv_word(id.span, "view", &["SRV", "UAV"]),
    }
    .parse_next(s)
}

/// Everything until `END` comes up on a line on its own.
///
/// Also parses following line ends.
fn until_end_literal<'a>(s: &mut Input<'a>) -> Result<&'a str> {
    alt((
        // Either nothing
        ("END", line_end).map(|_| ""),
        // Or with a following line break
        terminated(take_until(0.., "\nEND\n"), ("\nEND", line_end)),
        terminated(take_until(0.., "\nEND\r\n"), ("\nEND", line_end)),
        // Or until end of file
        rest.verify_map(|s: &str| s.strip_suffix("\nEND")),
    ))
    .parse_next(s)
    .map_err(|_| {
        ErrMode::Backtrack(ParserError::UndelimitedStatement {
            span: s.current_token_start()..s.current_token_start() + 1,
        })
    })
}

fn statements(s: &mut Input) -> Result<Vec<Directive>> {
    opt(line_end).parse_next(s)?;
    repeat(0.., statement).parse_next(s)
}

fn inv_word<'a, Output>(
    span: Range<usize>, name: &'static str, expected: &'static [&'static str],
) -> impl Parser<'a, Output> {
    move |_: &mut _| {
        Err(ErrMode::Backtrack(ParserError::InvalidWord { span: span.clone(), name, expected }))
    }
}

fn inv_statement(dir_format: &'static str, id: SpanObj<String>) -> impl Fn(Error) -> Error {
    move |e: Error| {
        e.map(|e| {
            if let ParserError::Unknown { span } = e {
                ParserError::InvalidStatement { decl: id.span.clone(), span, dir_format }
            } else {
                e
            }
        })
        .cut()
    }
}

fn statement(s: &mut Input) -> Result<Directive> {
    dispatch_id! {id;
        "ASSERT" => assert(id.clone())
            .map_err(inv_statement("ASSERT SHADERID [EQ|NE] <shaderid_a> <shaderid_b>", id)),
        "BLAS" => blas.map_err(inv_statement("BLAS <name> content... END", id)),
        "BUFFER" => buffer
            .map_err(inv_statement("BUFFER <name> [DATA_TYPE <tyflagspe> SIZE <elements> | RAW <bytes> ... END]", id)),
        "COLLECTION" => pso(PipelineStateObjectType::Collection)
            .map_err(inv_statement("COLLECTION name [ADDTO existing_name]", id)),
        "COMMAND_SIGNATURE" => command_signature
            .map_err(inv_statement("COMMAND_SIGNATURE <name> [STRIDE <stride>] [ROOT_SIG <root_sig>] content... END", id)),
        "DISPATCH" => dispatch(id.clone(), DispatchParseType::Dispatch)
            .map_err(inv_statement("DISPATCH <pipeline> content... RUN <x> <y> <z>", id)),
        "DISPATCHRAYS" => dispatch(id.clone(), DispatchParseType::DispatchRays)
            .map_err(inv_statement("DISPATCHRAYS <pipeline> content... RUN <raygentab> <misstab> <hittab> <calltab> <x> <y> <z>", id)),
        "DUMP" => dump(id.clone())
            .map_err(inv_statement("DUMP <resource> <type> [PRINT_STRIDE <stride>] [EXPECT]", id)),
        "EXECUTE_INDIRECT" => dispatch(id.clone(), DispatchParseType::ExecuteIndirect)
            .map_err(inv_statement("EXECUTE_INDIRECT <pipeline> SIGNATURE <command_signature> content... RUN <argument_buffer> [OFFSET <offset>] MAX_COMMANDS <num> [COUNT <count_buffer> [COUNT_OFFSET <offset>]]", id)),
        "EXPECT" => expect(id.clone())
            .map_err(inv_statement("EXPECT <resource> <type> [EPSILON <epsilon>] OFFSET <offset> EQ <list>", id)),
        "INCLUDE" => include(id.clone()).map_err(inv_statement("INCLUDE <path>", id)),
        "LIB" => object(false)
            .map_err(inv_statement("LIB <name> <source_name> <shader_model> [<compile arguments>]", id)),
        "OBJECT" => object(true)
            .map_err(inv_statement("OBJECT <name> <source_name> <shader_model> <entrypoint> [<compile arguments>]", id)),
        "PIPELINE" => pipeline
            .map_err(inv_statement("PIPELINE <name> <type> content... END", id)),
        "RTPSO" => pso(PipelineStateObjectType::Pipeline)
            .map_err(inv_statement("RTPSO name [ADDTO existing_name]", id)),
        "ROOT" => root_sig.map_err(inv_statement("ROOT <name> content... END", id)),
        "ROOT_DXIL" => root_sig_dxil.map_err(inv_statement("ROOT_DXIL <sig_name> <object>", id)),
        "SHADERTABLE" => shadertable
            .map_err(inv_statement("SHADERTABLE <name> <state_object> content... END", id)),
        "SHADERID" => shaderid.map_err(inv_statement("SHADERID <name> <state_object> <shader_name>", id)),
        "SLEEP" => sleep(id.clone()).map_err(inv_statement("SLEEP <duration>", id)),
        "SOURCE" => source.map_err(inv_statement("SOURCE <name> code... END", id)),
        "TLAS" => tlas.map_err(inv_statement("TLAS <name> content... END", id)),
        "VIEW" => view.map_err(inv_statement("VIEW <name> <buffer_name> AS [UAV|SRV|RTAS SRV]", id)),
        _ => cut_err(fail.map_err(|()| ErrMode::Backtrack(ParserError::UnknownStatement { span: id.span.clone() }))),
    }
    .parse_next(s)
}

fn object<'a>(is_object: bool) -> impl Parser<'a, Directive> {
    move |s: &mut Input<'a>| {
        let name = preceded(space, identifier).parse_next(s)?;
        let name = &name;
        let res = alt((
            // Either arguments or a DXIL code block
            seq! {Directive::Object {
                name: empty.value(name.clone()),
                _: space,
                source: identifier,
                _: space,
                shader_model: word.output_into(),
                entrypoint: |s: &mut Input<'a>| if is_object { preceded(space, word).map(|s| Some(s.to_string())).parse_next(s) } else { Ok(None) },
                args: repeat(0.., preceded(space, string)),
                _: line_end,
            }},
            seq! {Directive::ObjectDxil {
                name: empty.value(name.clone()),
                _: single_line_end,
                content: until_end_literal.output_into(),
            }},
        ))
        .parse_next(s)?;
        Ok(res)
    }
}

fn export(s: &mut Input) -> Result<Export> {
    seq! {Export {
        _: space,
        name: string_no_equals,
        to_rename: opt(preceded('=', string)),
    }}
    .parse_next(s)
}

fn pso_lib(s: &mut Input) -> Result<LinkObject> {
    seq! {LinkObject {
        _: space,
        name: identifier,
        exports: opt(preceded((space, "EXPORTS"), repeat(1.., export))).map(|o| o.unwrap_or_default()),
        _: line_end,
    }}
    .parse_next(s)
}

fn pso_hit_group(s: &mut Input) -> Result<HitGroup> {
    seq! {HitGroup {
        _: space,
        name: identifier,
        shaders: repeat(3, preceded(space, alt((none, identifier.map(Some))))).map(|v: Vec<_>| v.try_into().unwrap()),
        _: line_end,
    }}
    .parse_next(s)
}

fn pso<'a>(typ: PipelineStateObjectType) -> impl Parser<'a, Directive> {
    move |s: &mut Input<'a>| {
        let mut libs = Vec::new();
        let mut collections = Vec::new();
        let mut hit_groups = Vec::new();
        let mut config = Default::default();

        seq! {Directive::PipelineStateObject {
            _: space,
            name: identifier,
            typ: empty.value(typ),
            add_to: opt(preceded((space, "ADDTO", space), identifier)),
            _: line_end,
            _: repeat::<_, _, (), _, _>(0.., dispatch_id! {id;
                "COLLECTION" => pso_lib.map(|r| collections.push(r)).map_err(inv_statement("COLLECTION <obj> [EXPORTS <name>[=<exportToRename>] <....>]", id)),
                "LIB" => pso_lib.map(|r| libs.push(r)).map_err(inv_statement("LIB <obj> [EXPORTS <name>[=<exportToRename>] <....>]", id)),
                "HIT_GROUP" => pso_hit_group.map(|r| hit_groups.push(r)).map_err(inv_statement("HIT_GROUP <name> <anyhit> <closesthit> <intersection>", id)),
                "CONFIG" => cut_err(flags(&mut config, id)),
                _ => fail,
            }),
            libs: empty.value(mem::take(&mut libs)),
            collections: empty.value(mem::take(&mut collections)),
            hit_groups: empty.value(mem::take(&mut hit_groups)),
            config: empty.value(config.0.unwrap_or_default()),
            _: ("END", line_end),
        }}
        .parse_next(s)
    }
}

fn source(s: &mut Input) -> Result<Directive> {
    // Not using the seq! macro because we need to know the parser position at the start of the
    // content
    space.parse_next(s)?;
    let name = identifier.parse_next(s)?;
    single_line_end.parse_next(s)?;
    let position = s.current_token_start();
    let content = until_end_literal.parse_next(s)?;
    // Successfully parsed the Source statement
    // Now adjust the content with leading newlines to make shader compilation errors match the line
    // number in the original smoldr test file

    let checkpoint = s.input.checkpoint();
    s.input.reset_to_start();
    let line_count = memchr_iter(b'\n', s.input[..position].as_bytes()).count();
    let content = format!("{}{}", "\n".repeat(line_count), content);
    s.input.reset(&checkpoint);
    Ok(Directive::Source { name, content })
}

fn vertex(s: &mut Input) -> Result<Vertex> {
    Ok(Vertex(
        repeat(3, preceded(space, number::<f32>))
            .map(|v: Vec<_>| v.try_into().unwrap())
            .parse_next(s)?,
    ))
}

fn dim3(s: &mut Input) -> Result<Dim3> {
    Ok(Dim3(
        repeat(3, preceded(space, uint::<u32>))
            .map(|v: Vec<_>| v.try_into().unwrap())
            .parse_next(s)?,
    ))
}

fn aabb(s: &mut Input) -> Result<Aabb> {
    seq! {Aabb {
        min: vertex,
        max: vertex,
        _: line_end,
    }}
    .parse_next(s)
}

fn transform(s: &mut Input) -> Result<Transform> {
    let line_parser = terminated(separated(4, number::<f32>, space).map(|v: Vec<_>| v), line_end);
    let mut parser = delimited(
        line_end,
        repeat(3, line_parser)
            .map(|v: Vec<_>| v.into_iter().flatten().collect::<Vec<_>>().try_into().unwrap()),
        ("END", line_end),
    );
    Ok(Transform(parser.parse_next(s)?))
}

fn blas_geometry_procedural(s: &mut Input) -> Result<ProceduralGeometry> {
    let mut aabbs = Vec::new();
    let mut config = Default::default();

    line_end(s)?;

    repeat::<_, _, (), _, _>(0.., dispatch_id! {id;
        "AABB" => aabb.map(|r| aabbs.push(r)).map_err(inv_statement("AABB <min_x> <min_y> <min_z> <max_x> <max_y> <max_z>", id)),
        "CONFIG" => cut_err(flags(&mut config, id)),
        _ => fail,
    })
    .parse_next(s)?;

    ("END", line_end).parse_next(s)?;

    Ok(ProceduralGeometry { aabbs, config: config.0.unwrap_or_default() })
}

fn blas_geometry_triangle(s: &mut Input) -> Result<TriangleGeometry> {
    let mut vertices = Vec::new();
    let mut triangle_transform = Default::default();
    let mut config = Default::default();

    line_end(s)?;

    repeat::<_, _, (), _, _>(0.., dispatch_id! {id;
        "VERTEX" => terminated(vertex, line_end).map(|r| vertices.push(r)).map_err(inv_statement("VERTEX <x> <y> <z>", id)),
        "TRANSFORM" => no_dup(&mut triangle_transform, id.clone(), transform).map_err(inv_statement("TRANSFORM content 3x4... END", id)),
        "CONFIG" => cut_err(flags(&mut config, id)),
        _ => fail,
    })
    .parse_next(s)?;

    ("END", line_end).parse_next(s)?;

    Ok(TriangleGeometry {
        vertices,
        transform: triangle_transform.0,
        config: config.0.unwrap_or_default(),
    })
}

fn blas(s: &mut Input) -> Result<Directive> {
    let mut procedurals = Vec::new();
    let mut triangles = Vec::new();
    let mut config = Default::default();

    seq! {Directive::Blas {
        _: space,
        name: identifier,
        _: line_end,
        _: repeat::<_, _, (), _, _>(0.., dispatch_id! {id;
                "GEOMETRY" => cut_err(preceded(space, dispatch_no_move! {word;
                    "PROCEDURAL" => cut_err(blas_geometry_procedural.map(|r| procedurals.push(r))),
                    "TRIANGLE" => cut_err(blas_geometry_triangle.map(|r| triangles.push(r))),
                    _ => fail,
                })),
                "CONFIG" => cut_err(flags(&mut config, id)),
                _ => fail,
            }),
        procedurals: empty.value(mem::take(&mut procedurals)),
        triangles: empty.value(mem::take(&mut triangles)),
        config: empty.value(config.0.unwrap_or_default()),
        _: ("END", line_end),
    }}
    .parse_next(s)
}

fn tlas_blas(s: &mut Input) -> Result<TlasBlas> {
    let mut id_field = Default::default();
    let mut mask = Default::default();
    let mut index_contrib = Default::default();
    let mut blas_transform = Default::default();
    let mut config = Default::default();

    seq! {TlasBlas {
        _: space,
        name: identifier,
        _: alt((
            (space, none::<()>, line_end).void(),
            (
                line_end,
                repeat::<_, _, (), _, _>(0.., dispatch_id! {id;
                    "ID" => no_dup(&mut id_field, id.clone(), delimited(space, uint, line_end)).map_err(inv_statement("ID <num>", id)),
                    "MASK" => no_dup(&mut mask, id.clone(), delimited(space, uint, line_end)).map_err(inv_statement("MASK <num>", id)),
                    "HIT_GROUP_INDEX_CONTRIBUTION" => no_dup(&mut index_contrib, id.clone(), delimited(space, uint, line_end)).map_err(inv_statement("HIT_GROUP_INDEX_CONTRIBUTION <num>", id)),
                    "TRANSFORM" => no_dup(&mut blas_transform, id.clone(), transform).map_err(inv_statement("TRANSFORM content 3x4... END", id)),
                    "CONFIG" => cut_err(flags(&mut config, id)),
                    _ => fail,
                }),
                ("END", line_end),
            ).void(),
        )),
        id: empty.value(mem::take(&mut id_field.0)),
        mask: empty.value(mem::take(&mut mask.0)),
        index_contrib: empty.value(mem::take(&mut index_contrib.0)),
        transform: empty.value(mem::take(&mut blas_transform.0)),
        config: empty.value(config.0.unwrap_or_default()),
    }}
    .parse_next(s)
}

fn tlas(s: &mut Input) -> Result<Directive> {
    let mut blas = Vec::new();
    let mut config = Default::default();

    seq! {Directive::Tlas {
        _: space,
        name: identifier,
        _: line_end,
        _: repeat::<_, _, (), _, _>(0.., dispatch_id! {id;
                "BLAS" => tlas_blas.map(|r| blas.push(r)).map_err(inv_statement("BLAS <name> [-]", id)),
                "CONFIG" => cut_err(flags(&mut config, id.clone())),
                _ => fail,
            }),
        blas: empty.value(mem::take(&mut blas)),
        config: empty.value(config.0.unwrap_or_default()),
        _: ("END", line_end),
    }}
    .parse_next(s)
}

fn fill_series<'a>(typ: DataType) -> impl Parser<'a, Fill> {
    move |s: &mut Input<'a>| {
        seq! {Fill::Series {
            _: space,
            from: value(typ),
            _: (space, "INC_BY", space),
            increment: value(typ),
        }}
        .parse_next(s)
    }
}

fn fill_const<'a>(typ: DataType) -> impl Parser<'a, Fill> {
    move |s: &mut Input<'a>| Ok(Fill::Const(preceded(space, value(typ)).parse_next(s)?))
}

fn content_typed(s: &mut Input) -> Result<UnresolvedValueContent> {
    Ok(UnresolvedValueContent::Resolved(
        seq! {ValueContent::Typed {
            _: space,
            typ: data_type,
            _: (space, "SIZE", space),
            element_count: uint,
            _: space,
            fill: dispatch_id! {id;
                "SERIES_FROM" => fill_series(typ).map_err(inv_statement("SERIES_FROM <start> INC_BY <increment>", id)),
                "FILL" => fill_const(typ).map_err(inv_statement("FILL <const>", id)),
                _ => fail,
            },
            _: line_end,
        }}
        .parse_next(s)?,
    ))
}

fn content_raw(s: &mut Input) -> Result<UnresolvedValueContent> {
    space(s)?;
    let size = span_obj(uint::<u64>).parse_next(s)?;
    line_end(s)?;
    let data = span_obj(
        repeat(
            0..,
            alt((
                terminated(preceded(("GPUVA", space), identifier), line_end)
                    .map(|i| vec![UnresolvedValue::Gpuva(i)]),
                |s: &mut Input| {
                    let typ = data_type(s)?;
                    let r = repeat(1.., preceded(space, value(typ).map(UnresolvedValue::Resolved)))
                        .parse_next(s)?;
                    line_end(s)?;
                    Ok(r)
                },
            )),
        )
        .map(|v: Vec<_>| v.into_iter().flatten().collect()),
    )
    .parse_next(s)?;

    ("END", line_end).parse_next(s)?;

    let content = UnresolvedValueContent::Raw { values: UnresolvedValues { data: data.content } };

    // Check size
    if usize::try_from(size.content).unwrap() != content.len() {
        return Err(ErrMode::Backtrack(ParserError::RawSizeMismatch {
            size,
            content: data.span,
            content_size: content.len(),
        }));
    }

    Ok(content)
}

fn content(s: &mut Input) -> Result<UnresolvedValueContent> {
    dispatch! {word;
        "DATA_TYPE" => cut_err(content_typed),
        "RAW" => cut_err(content_raw),
        _ => fail,
    }
    .parse_next(s)
}

fn buffer(s: &mut Input) -> Result<Directive> {
    seq! {Directive::Buffer {
        _: space,
        name: identifier,
        _: space,
        content: content,
    }}
    .parse_next(s)
}

fn include<'a>(mut id: Identifier) -> impl Parser<'a, Directive> {
    move |s: &mut Input<'a>| {
        space(s)?;
        let path = span_obj(string).parse_next(s)?;
        line_end(s)?;

        Ok(Directive::Include { identifier: mem::take(&mut id), path })
    }
}

fn root_sig_table(s: &mut Input) -> Result<RootSigTable> {
    seq! {RootSigTable {
        _: space,
        typ: view_type,
        _: (space, "REGISTER", space),
        register: uint,
        _: (space, "NUMBER", space),
        number: uint,
        _: (space, "SPACE", space),
        space: uint,
        _: line_end,
    }}
    .parse_next(s)
}

fn root_sig_view<'a>(typ: ViewType) -> impl Parser<'a, RootSigView> {
    move |s: &mut Input<'a>| {
        seq! {RootSigView {
            typ: empty.value(typ),
            _: (space, "REGISTER", space),
            register: uint,
            _: (space, "SPACE", space),
            space: uint,
            _: line_end,
        }}
        .parse_next(s)
    }
}

fn root_sig_const(s: &mut Input) -> Result<RootSigConst> {
    seq! {RootSigConst {
        _: (space, "NUMBER", space),
        number: uint,
        _: (space, "REGISTER", space),
        register: uint,
        _: (space, "SPACE", space),
        space: uint,
        _: line_end,
    }}
    .parse_next(s)
}

fn root_sig(s: &mut Input) -> Result<Directive> {
    let mut entries = Vec::new();
    let mut config = Default::default();

    seq! {Directive::RootSig {
        _: space,
        name: identifier,
        _: line_end,
        _: repeat::<_, _, (), _, _>(0.., dispatch_id! {id;
                "TABLE" => root_sig_table.map(|r| entries.push(RootSigEntry::Table(r)))
                    .map_err(inv_statement("TABLE <type> REGISTER <num> NUMBER <num> SPACE <space>",
                        id)),
                "SRV" => root_sig_view(ViewType::Srv).map(|r| entries.push(RootSigEntry::View(r)))
                    .map_err(inv_statement("SRV REGISTER <num> SPACE <space>", id)),
                "UAV" => root_sig_view(ViewType::Uav).map(|r| entries.push(RootSigEntry::View(r)))
                    .map_err(inv_statement("UAV REGISTER <num> SPACE <space>", id)),
                "ROOT_CONST" => root_sig_const.map(|r| entries.push(RootSigEntry::Const(r)))
                    .map_err(inv_statement("ROOT_CONST COUNT <num> REGISTER <num> SPACE <space>", id)),
                "CONFIG" => cut_err(flags(&mut config, id)),
                _ => fail,
            }),
        entries: empty.value(mem::take(&mut entries)),
        config: empty.value(config.0.unwrap_or_default()),
        _: ("END", line_end),
    }}
    .parse_next(s)
}

fn root_sig_dxil(s: &mut Input) -> Result<Directive> {
    seq! {Directive::RootSigDxil {
        _: space,
        name: identifier,
        _: space,
        object: identifier,
        _: line_end
    }}
    .parse_next(s)
}

fn pipeline(s: &mut Input) -> Result<Directive> {
    let mut shaders = Vec::new();
    let mut root_sig = Default::default();

    seq! {Directive::Pipeline {
        _: space,
        name: identifier,
        _: space,
        typ: dispatch_id! {id;
            "COMPUTE" => empty.value(PipelineType::Compute),
            _ => inv_word(id.span, "pipeline type", &["COMPUTE"]),
        },
        _: line_end,
        _: repeat::<_, _, (), _, _>(0.., dispatch_id! {id;
                "ATTACH" => delimited(space, identifier, line_end).map(|r| shaders.push(r)).map_err(inv_statement("ATTACH <name>", id)),
                "ROOT" => no_dup(&mut root_sig, id.clone(), delimited(space, identifier, line_end)).map_err(inv_statement("ROOT <root_sig>", id)),
                _ => fail,
            }),
        shaders: empty.value(mem::take(&mut shaders)),
        root_sig: empty.value(mem::take(&mut root_sig.0)),
        _: ("END", line_end),
    }}
    .parse_next(s)
}

fn view(s: &mut Input) -> Result<Directive> {
    seq! {Directive::View {
        _: space,
        name: identifier,
        _: space,
        buffer: identifier.map(Some),
        _: (space, "AS", space),
        typ: alt((
            preceded("TYPED", (preceded(space, view_type), preceded(space, data_type))
                .map(|(typ, data_type)| InputViewType::Typed { typ, data_type })),
            preceded("STRUCTURED", (preceded(space, view_type), preceded((space, "BYTES", space), uint))
                .map(|(typ, struct_size)| InputViewType::Structured { typ, struct_size })),
            view_type.map(|typ| InputViewType::Raw { typ }),
            ("RTAS", space, "SRV").map(|_| InputViewType::RaytracingAccelStruct),
        )),
        _: line_end,
    }}
    .parse_next(s)
}

fn record_content<'a, 'b, 'c>(
    shader: &'b mut (Option<ShaderReference>, Option<Identifier>),
    root_val: &'c mut UnresolvedRootVal,
) -> impl Parser<'a, ()> + use<'a, 'b, 'c> {
    move |s: &mut Input<'a>| {
        line_end(s)?;
        let mut i_num = 0;
        let mut i = || {
            i_num += 1;
            i_num - 1
        };
        repeat(0.., dispatch_id! {id;
            "TABLE" => cut_err(delimited(space, identifier, line_end))
                .map(|r| root_val.binds.push(Bind { index: i(), view: r })),
            "GPUVA" => cut_err(delimited(space, identifier, line_end))
                .map(|r| root_val.views.push(RootValView { index: i(), typ: None, buffer: r })),
            "SHADERID" => cut_err(delimited(space, no_dup(shader, id, identifier.map(ShaderReference::ShaderId)), line_end)),
            _ => fail,
        })
        .map(|()| ())
        .parse_next(s)?;

        ("END", line_end).parse_next(s)?;

        Ok(())
    }
}

fn record(s: &mut Input) -> Result<UnresolvedShaderTableRecord> {
    let mut shader = Default::default();
    let mut root_val = Default::default();

    seq! {UnresolvedShaderTableRecord {
        _: space,
        index: uint,
        _: alt((
            |s: &mut _| {
                space(s)?;
                let shader_id = span_obj(string).parse_next(s)?;
                let mut shader = (Some(ShaderReference::Name(shader_id.clone())), Some(shader_id));
                let mut root_val = Default::default();
                alt(((space, none::<()>, line_end).void(), record_content(&mut shader, &mut root_val))).parse_next(s)?;
                Ok((shader, root_val))
            },
            |s: &mut _| {
                let mut shader = Default::default();
                let mut root_val = Default::default();
                record_content(&mut shader, &mut root_val).parse_next(s)?;
                Ok((shader, root_val))
            },
        )).map(|(s, r)| { shader = s.0.unwrap_or_default(); root_val = r; }),
        shader: empty.value(mem::take(&mut shader)),
        root_val: empty.value(mem::take(&mut root_val)),
    }}
    .parse_next(s)
}

fn shadertable(s: &mut Input) -> Result<Directive> {
    seq! {Directive::ShaderTable {
        _: space,
        name: identifier,
        _: space,
        pipeline_state_object: identifier,
        _: line_end,
        records: repeat(0.., preceded("RECORD", cut_err(record))),
        _: ("END", line_end),
    }}
    .parse_next(s)
}

fn shaderid(s: &mut Input) -> Result<Directive> {
    seq! {Directive::ShaderId {
        _: space,
        name: identifier,
        _: space,
        pipeline_state_object: identifier,
        _: space,
        shader_name: span_obj(string),
        _: line_end
    }}
    .parse_next(s)
}

fn dispatch_view<'a>(typ: ViewType) -> impl Parser<'a, RootValView> {
    move |s: &mut Input<'a>| {
        seq! {RootValView {
            _: space,
            index: uint,
            _: space,
            typ: empty.value(Some(typ)),
            buffer: identifier,
            _: line_end,
        }}
        .parse_next(s)
    }
}

fn dispatch<'a>(mut id: Identifier, ty: DispatchParseType) -> impl Parser<'a, Directive> {
    use identifier as parse_identifier;
    move |s: &mut Input<'a>| {
        let mut id = mem::take(&mut id);
        let mut root_sig = Default::default();
        let mut root_val = UnresolvedRootVal::default();

        // Execute indirect
        let mut signature = None;

        seq! {Directive::Dispatch {
            identifier: empty.value(mem::take(&mut id)),
            _: space,
            pipeline: parse_identifier,
            _: |s: &mut _| if ty == DispatchParseType::ExecuteIndirect {
                (space, "SIGNATURE", space).parse_next(s)?;
                signature = Some(parse_identifier(s)?);
                Ok(())
            } else { Ok(()) },
            _: line_end,
            _: repeat(0.., dispatch_id! {id;
                "BIND" => seq! {Bind {
                    _: space,
                    index: uint,
                    _: (space, "TABLE", space),
                    view: parse_identifier,
                    _: line_end,
                }}
                .map(|r| root_val.binds.push(r))
                .map_err(inv_statement("BIND <index> [TABLE <view>]", id)),
                "ROOT_CONST" => seq! {UnresolvedRootValConst {
                    _: space,
                    index: uint,
                    _: space,
                    content: content,
                }}
                .map(|r| root_val.consts.push(r))
                .map_err(inv_statement("ROOT_CONST <idx> <data_spec>", id)),
                "SRV" => dispatch_view(ViewType::Srv).map(|r| root_val.views.push(r)).map_err(inv_statement("SRV <idx> <buffer_name>", id)),
                "UAV" => dispatch_view(ViewType::Uav).map(|r| root_val.views.push(r)).map_err(inv_statement("UAV <idx> <buffer_name>", id)),
                "ROOT_SIG" => delimited(space, no_dup(&mut root_sig, id.clone(), parse_identifier), line_end).map_err(inv_statement("ROOT_SIG <name>", id)),
                _ => fail,
            }).map(|()| ()),
            root_val: empty.value(mem::take(&mut root_val)),
            root_sig: empty.value(mem::take(&mut root_sig.0)),
            _: "RUN",
            typ:|s: &mut _|  {
                let mut signature = mem::take(&mut signature);
                let mut count_buffer = None;
                let mut count_offset = None;
                match ty {
                    DispatchParseType::Dispatch => seq! {DispatchType::Dispatch { dimensions: terminated(dim3, line_end) }}.parse_next(s),
                    DispatchParseType::DispatchRays => seq! {DispatchType::DispatchRays {
                        tables: repeat(4, preceded(space, alt((none, parse_identifier.map(Some))))).map(|r: Vec<_>| r.try_into().unwrap()),
                        dimensions: terminated(dim3, line_end),
                    }}.parse_next(s),
                    DispatchParseType::ExecuteIndirect => seq! {DispatchType::ExecuteIndirect {
                        signature: empty.value(mem::take(&mut signature).unwrap()),
                        _: space,
                        argument_buffer: parse_identifier,
                        argument_offset: opt(preceded((space, "OFFSET", space), uint)),
                        max_commands: preceded((space, "MAX_COMMANDS", space), uint),
                        _: opt((space, "COUNT", space, parse_identifier.map(|r| count_buffer = Some(r)),
                            opt((space, "COUNT_OFFSET", space, uint.map(|r| count_offset = Some(r)))))),
                        count_buffer: empty.value(mem::take(&mut count_buffer)),
                        count_offset: empty.value(mem::take(&mut count_offset)),
                        _: line_end,
                    }}.parse_next(s),
                }
            },
        }}
        .parse_next(s)
    }
}

fn sleep<'a>(mut id: Identifier) -> impl Parser<'a, Directive> {
    move |s: &mut Input<'a>| {
        space(s)?;
        let duration_span: Identifier =
            span_obj(take_while(1.., ('a'..='z', '0'..='9', ' ')).output_into()).parse_next(s)?;
        line_end(s)?;

        let duration = match humantime::parse_duration(&duration_span.content) {
            Ok(d) => duration_span.map_content(d),
            Err(source) => {
                return Err(ErrMode::Backtrack(ParserError::ParseDuration {
                    source,
                    duration_span: duration_span.span,
                    identifier: id.clone(),
                }));
            }
        };

        Ok(Directive::Sleep { identifier: mem::take(&mut id), duration })
    }
}

fn dump<'a>(mut id: Identifier) -> impl Parser<'a, Directive> {
    use identifier as parse_identifier;
    move |s: &mut Input<'a>| {
        let mut id = mem::take(&mut id);
        seq! {Directive::Dump {
            identifier: empty.value(mem::take(&mut id)),
            _: space,
            resource: parse_identifier,
            _: space,
            typ: alt((data_type.map(DumpDataType::DataType), "DXIL".value(DumpDataType::Dxil))),
            print_stride: opt(preceded((space,"PRINT_STRIDE", space), non_zero_u64)),
            format: opt((space, "EXPECT")).map(|r| if r.is_some() { DumpFormat::Expect } else { DumpFormat::List }),
            _: line_end,
        }}
        .parse_next(s)
    }
}

fn expect<'a>(mut id: Identifier) -> impl Parser<'a, Directive> {
    use identifier as parse_identifier;
    move |s: &mut Input<'a>| {
        let mut id = mem::take(&mut id);
        let mut typ = None;
        let mut value_spans = Vec::new();

        seq! {Directive::Expect {
            identifier: empty.value(mem::take(&mut id)),
            _: space,
            resource: parse_identifier,
            _: space,
            _: data_type.map(|r| typ = Some(r)),
            epsilon: opt(preceded((space, "EPSILON", space), cut_err(span_obj(value(typ.unwrap()))))),
            _: (space, "OFFSET", space),
            offset: span_obj(uint),
            _: (space, "EQ"),
            values: repeat(1.., preceded(space, span_obj(value(typ.unwrap())).with_taken())
                .map(|(v, s)| { value_spans.push(v.map_content(s.to_string())); UnresolvedValue::Resolved(v.content) }))
                .map(|v: Vec<_>| UnresolvedValues { data: v }),
            value_spans: empty.value(mem::take(&mut value_spans)),
            _: line_end,
        }}
        .parse_next(s)
    }
}

fn assert<'a>(mut id: Identifier) -> impl Parser<'a, Directive> {
    use identifier as parse_identifier;
    move |s: &mut Input<'a>| {
        let mut id = mem::take(&mut id);
        seq! {Directive::AssertShaderId {
            identifier: empty.value(mem::take(&mut id)),
            _: (space, "SHADERID", space),
            equal: alt(("EQ".value(true), "NE".value(false))),
            _: space,
            id_a: parse_identifier,
            _: space,
            id_b: parse_identifier,
            _: line_end,
        }}
        .parse_next(s)
    }
}

fn command_signature(s: &mut Input) -> Result<Directive> {
    seq! {Directive::CommandSignature {
        _: space,
        name: identifier,
        stride: opt(preceded((space, "STRIDE", space), uint)),
        root_sig: opt(preceded((space, "ROOT_SIG", space), identifier)),
        _: line_end,
        arguments: repeat(0.., terminated(dispatch! {word;
            "SRV" => cut_err(preceded((space, "REGISTER", space), uint.map(|r| CommandSignatureArgument::View { typ: ViewType::Srv, index: r }))),
            "UAV" => cut_err(preceded((space, "REGISTER", space), uint.map(|r| CommandSignatureArgument::View { typ: ViewType::Uav, index: r }))),
            "ROOT_CONST" => cut_err(seq! {CommandSignatureArgument::Constant {
                _: (space, "NUMBER", space),
                number: uint,
                _: (space, "REGISTER", space),
                index: uint,
                _: (space, "OFFSET", space),
                offset: uint,
            }}),
            "DISPATCH" => empty.value(CommandSignatureArgument::Dispatch),
            "DISPATCHRAYS" => empty.value(CommandSignatureArgument::DispatchRays),
            _ => fail,
        }, line_end)),
        _: ("END", line_end),
    }}
    .parse_next(s)
}

#[cfg(test)]
mod code_tests {
    use super::*;

    fn input(s: &str) -> Input<'_> {
        Stateful { input: LocatingSlice::new(s), state: Default::default() }
    }

    fn test_parse<'a, Output>(
        mut parser: impl Parser<'a, Output>, s: &'a str,
    ) -> Result<Output, ParserError> {
        parser.parse(input(s)).map_err(|e| e.into_inner())
    }

    #[test]
    fn test_singleline_comment() {
        test_parse(singleline_comment, "# comment").unwrap();
        test_parse(singleline_comment, "#").unwrap();
        test_parse(singleline_comment, "// comment").unwrap();
        test_parse(singleline_comment, "///* comment */").unwrap();
        test_parse(singleline_comment, "//* comment */").unwrap();
        test_parse(singleline_comment, "//").unwrap();
        // You can comment out a multiline comment start
        test_parse(singleline_comment, "// test /* comment").unwrap();
    }

    #[test]
    fn test_multiline_comment() {
        test_parse(multiline_comment, "/* comment */").unwrap();
        test_parse(multiline_comment, "/* comment\nline */").unwrap();
        test_parse(multiline_comment, "/* c /* nested */ end */").unwrap();
        let res = test_parse(multiline_comment, "/* c /* nested */ end/");
        assert!(res.is_err());
        test_parse(multiline_comment, "/* c * nested / end */").unwrap();
    }

    #[test]
    fn test_space() {
        test_parse(space, "   \t  ").unwrap();
        test_parse(space, " ").unwrap();
        test_parse(space, " /* comment */ ").unwrap();
        let res = test_parse(space, "");
        assert!(res.is_err());
        let res = test_parse(space, "\n");
        assert!(res.is_err());
    }

    #[test]
    fn test_line_end() {
        test_parse(line_end, "\n").unwrap();
        test_parse(line_end, "\r\n").unwrap();
        test_parse(line_end, "\n  ").unwrap();
        test_parse(line_end, "   \t  \n").unwrap();
        test_parse(line_end, "").unwrap();
        test_parse(line_end, " #").unwrap();
        test_parse(line_end, " #\n").unwrap();
        test_parse(line_end, " #\n  ").unwrap();
        test_parse(line_end, " #\n /* c */ \n").unwrap();
        test_parse(line_end, " #\n    \t#").unwrap();
        test_parse(line_end, " //\n").unwrap();
        test_parse(line_end, " //n  ").unwrap();
        test_parse(line_end, " //\n /* c */ \n").unwrap();
        test_parse(line_end, " //\n    \t//").unwrap();
        let res = test_parse(line_end, " #\na");
        assert!(res.is_err());
        let res = test_parse(line_end, "a");
        assert!(res.is_err());
    }

    #[test]
    fn test_identifier() {
        let res = test_parse(identifier, "");
        assert!(res.is_err());
        let res = test_parse(identifier, "0123");
        assert!(res.is_err());
        let res = test_parse(identifier, "b");
        assert_eq!(res.unwrap().content, "b");
        let res = test_parse(identifier, "a0123");
        assert_eq!(res.unwrap().content, "a0123");
        let res = test_parse(identifier, "_a0123");
        assert_eq!(res.unwrap().content, "_a0123");
    }

    #[test]
    fn test_uint() {
        let res = test_parse(uint::<u32>, "");
        assert!(res.is_err());
        let res = test_parse(uint::<u32>, "-1");
        assert!(res.is_err());
        let res = test_parse(uint::<u32>, "0");
        assert_eq!(res.unwrap(), 0);
        let res = test_parse(uint::<u32>, "0123");
        assert_eq!(res.unwrap(), 123);
        let res = test_parse(uint::<u32>, "0x0123");
        assert_eq!(res.unwrap(), 0x123);
        let res = test_parse(uint::<u32>, "0b101");
        assert_eq!(res.unwrap(), 0b101);
    }

    #[test]
    fn test_number() {
        let res = test_parse(number::<i32>, "");
        assert!(res.is_err());
        let res = test_parse(number::<i32>, "0");
        assert_eq!(res.unwrap(), 0);
        let res = test_parse(number::<i32>, "0123");
        assert_eq!(res.unwrap(), 123);
        let res = test_parse(number::<i32>, "0x0123");
        assert_eq!(res.unwrap(), 0x123);
        let res = test_parse(number::<i32>, "0b101");
        assert_eq!(res.unwrap(), 0b101);
        let res = test_parse(number::<i32>, "-0123");
        assert_eq!(res.unwrap(), -123);
        let res = test_parse(number::<i32>, "-0x0123");
        assert_eq!(res.unwrap(), -0x123);
        let res = test_parse(number::<f32>, "-0.5");
        assert_eq!(res.unwrap(), -0.5);
        let res = test_parse(number::<f64>, "-0.5");
        assert_eq!(res.unwrap(), -0.5);
    }

    #[test]
    fn test_string() {
        let res = test_parse(string, "");
        assert!(res.is_err());
        let res = test_parse(string, " ");
        assert!(res.is_err());
        let res = test_parse(string_no_equals, "=");
        assert!(res.is_err());
        let res = test_parse(string, "=");
        assert_eq!(res.unwrap().as_str(), "=");
        let res = test_parse(string, "0");
        assert_eq!(res.unwrap().as_str(), "0");
        let res = test_parse(string, "a");
        assert_eq!(res.unwrap().as_str(), "a");
        let res = test_parse(string, "\\\\");
        assert_eq!(res.unwrap().as_str(), "\\");
        let res = test_parse(string, "\"a\"");
        assert_eq!(res.unwrap().as_str(), "a");
        let res = test_parse(string, "\"\\\\a\"");
        assert_eq!(res.unwrap().as_str(), "\\a");
        let res = test_parse(string, "\"\\\"a\"");
        assert_eq!(res.unwrap().as_str(), "\"a");
        let res = test_parse(string, "\"\\00a\"");
        assert_eq!(res.unwrap().as_str(), "\0a");
        let res = test_parse(string, "\"\\0a\"");
        assert_eq!(res.unwrap().as_str(), "\x0a");
    }

    #[test]
    fn test_source() {
        let Directive::Source { name, content } =
            source.parse_next(&mut input(" source\ncontent\nEND")).unwrap()
        else {
            panic!()
        };
        assert_eq!(name.content, "source");
        assert_eq!(content, "\ncontent");

        source.parse_next(&mut input(" source\n\nEND")).unwrap();
        source.parse_next(&mut input(" source\n\nEND\n")).unwrap();
        test_parse(statements, "SOURCE source\n\nEND").unwrap();
        test_parse(statements, "SOURCE source\r\n\r\nmysource\r\nEND\r\n").unwrap();

        let Directive::Source { name, content } =
            source.parse_next(&mut input(" source\n \n   \ncont\nent\nEND")).unwrap()
        else {
            panic!()
        };
        assert_eq!(name.content, "source");
        assert_eq!(content, "\n \n   \ncont\nent");
    }
}
