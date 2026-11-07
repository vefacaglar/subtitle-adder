//! argparse-compatible parser for `subtitle-engine` (spec cli.md §2): unique-prefix long options, `=` forms,
//! `-ovalue`, greedy `nargs="+"`, known-args leftovers reported by the top-level parser, verbatim usage/help
//! texts at width 80.
//!
//! The algorithm is a line-by-line port of CPython 3.11.18 `argparse.ArgumentParser._parse_known_args`
//! (classification into `O`/`A`/`-`, alternating `consume_optional` / `consume_positionals`, the subparsers
//! action parsing the rest with known-args semantics) restricted to the action kinds `cli.py` uses.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use se_core::pyfmt::repr_str;

use crate::pyparse::{py_float, py_int};
use crate::texts::*;

pub use crate::texts::TOP_USAGE;

/// Parsed command line.
#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    /// Global `--lang` (before the subcommand).
    pub lang: Option<String>,
    /// Subcommand-level `--lang` (wins when non-empty).
    pub sub_lang: Option<String>,
    /// Global `--offline`.
    pub offline: bool,
    pub command: Command,
}

impl Args {
    /// The effective `--lang` value (`sub_lang or lang`, Python truthiness: `""` counts as missing).
    pub fn effective_lang(&self) -> Option<&str> {
        match self.sub_lang.as_deref() {
            Some(s) if !s.is_empty() => Some(s),
            _ => self.lang.as_deref(),
        }
    }

    /// `getattr(args, "json", False)`.
    pub fn json(&self) -> bool {
        match &self.command {
            Command::ExtractAudio { json, .. }
            | Command::Preview { json, .. }
            | Command::Transcribe { json, .. }
            | Command::Translate { json, .. }
            | Command::Models { json }
            | Command::DeleteModel { json, .. } => *json,
            Command::Render { .. } => false,
        }
    }
}

/// The subcommands with their options (defaults as in `cli.py`).
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    ExtractAudio { video: PathBuf, out_dir: PathBuf, json: bool },
    /// `video == "-"` (compared before any resolve) means the open file on stdin.
    Preview { video: PathBuf, output: PathBuf, json: bool },
    Transcribe {
        video: PathBuf,
        out_dir: PathBuf,
        language: Option<String>,
        model: String,
        /// Duplicates kept, in order.
        formats: Vec<String>,
        /// Python int (any size the parser accepts; `i64` here).
        max_chars: i64,
        max_duration: f64,
        diarize: bool,
        speakers: Option<i64>,
        json: bool,
    },
    Translate { to: String, source: Option<String>, model: String, json: bool },
    Render { format: String, out: Option<PathBuf> },
    Models { json: bool },
    DeleteModel { id: String, json: bool },
}

/// Result of parsing: run, or exit immediately with the given streams' text (help/version → stdout, exit 0;
/// usage error → stderr, exit 2).
#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    Run(Args),
    Exit { code: i32, stdout: String, stderr: String },
}

// ---------------------------------------------------------------------------------------------------------
// Parser definitions

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nargs {
    /// `store_true`, help, version (`nargs=0`).
    Zero,
    /// `nargs=None`.
    One,
    /// `nargs="+"`.
    OneOrMore,
    /// The subparsers action (`nargs=PARSER`).
    Parser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Store,
    StoreTrue,
    Help,
    Version,
    SubParsers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ty {
    Str,
    Int,
    Float,
}

#[derive(Clone, Debug)]
enum Val {
    None,
    Bool(bool),
    Str(String, OsString),
    List(Vec<(String, OsString)>),
    Int(i64),
    Float(f64),
}

struct Action {
    strings: &'static [&'static str],
    dest: &'static str,
    kind: Kind,
    nargs: Nargs,
    ty: Ty,
    choices: Option<&'static [&'static str]>,
    required: bool,
    default: fn() -> Val,
}

impl Action {
    fn is_optional(&self) -> bool {
        !self.strings.is_empty()
    }

    /// `_get_action_name`.
    fn name(&self) -> String {
        if self.is_optional() { self.strings.join("/") } else { self.dest.to_string() }
    }
}

struct ParserDef {
    prog: &'static str,
    usage: &'static str,
    help: &'static str,
    actions: Vec<Action>,
}

const FORMATS: &[&str] = &["srt", "vtt", "json"];
const COMMANDS: &[&str] = &["extract-audio", "preview", "transcribe", "translate", "render", "models", "delete-model"];

fn none() -> Val {
    Val::None
}
fn falsy() -> Val {
    Val::Bool(false)
}
fn path_output() -> Val {
    Val::Str("output".into(), OsString::from("output"))
}

fn opt(strings: &'static [&'static str], dest: &'static str) -> Action {
    Action { strings, dest, kind: Kind::Store, nargs: Nargs::One, ty: Ty::Str, choices: None, required: false, default: none }
}
fn flag(strings: &'static [&'static str], dest: &'static str) -> Action {
    Action { kind: Kind::StoreTrue, nargs: Nargs::Zero, default: falsy, ..opt(strings, dest) }
}
fn positional(dest: &'static str) -> Action {
    Action { required: true, ..opt(&[], dest) }
}
fn help_action() -> Action {
    Action { kind: Kind::Help, nargs: Nargs::Zero, ..opt(&["-h", "--help"], "help") }
}

fn top_parser() -> ParserDef {
    ParserDef {
        prog: "subtitle-engine",
        usage: TOP_USAGE,
        help: TOP_HELP,
        actions: vec![
            help_action(),
            opt(&["--lang"], "lang"),
            flag(&["--offline"], "offline"),
            Action { kind: Kind::Version, nargs: Nargs::Zero, ..opt(&["--version"], "version") },
            Action {
                kind: Kind::SubParsers,
                nargs: Nargs::Parser,
                choices: Some(COMMANDS),
                required: true,
                ..opt(&[], "command")
            },
        ],
    }
}

fn sub_parser(name: &str) -> ParserDef {
    let mut actions = vec![help_action(), opt(&["--lang"], "sub_lang")];
    let (prog, usage, help) = match name {
        "extract-audio" => {
            actions.push(positional("video"));
            actions.push(Action { default: path_output, ..opt(&["-o", "--out-dir"], "out_dir") });
            actions.push(flag(&["--json"], "json"));
            ("subtitle-engine extract-audio", EXTRACT_AUDIO_USAGE, EXTRACT_AUDIO_HELP)
        }
        "preview" => {
            actions.push(positional("video"));
            actions.push(Action { required: true, ..opt(&["--output"], "output") });
            actions.push(flag(&["--json"], "json"));
            ("subtitle-engine preview", PREVIEW_USAGE, PREVIEW_HELP)
        }
        "transcribe" => {
            actions.push(positional("video"));
            actions.push(Action { default: path_output, ..opt(&["-o", "--out-dir"], "out_dir") });
            actions.push(opt(&["-l", "--language"], "language"));
            actions.push(Action { default: || Val::Str("small".into(), "small".into()), ..opt(&["-m", "--model"], "model") });
            actions.push(Action {
                nargs: Nargs::OneOrMore,
                choices: Some(FORMATS),
                default: || Val::List(vec![("srt".into(), "srt".into())]),
                ..opt(&["-f", "--format"], "format")
            });
            actions.push(Action { ty: Ty::Int, default: || Val::Int(84), ..opt(&["--max-chars"], "max_chars") });
            actions.push(Action { ty: Ty::Float, default: || Val::Float(6.0), ..opt(&["--max-duration"], "max_duration") });
            actions.push(flag(&["--diarize"], "diarize"));
            actions.push(Action { ty: Ty::Int, ..opt(&["--speakers"], "speakers") });
            actions.push(flag(&["--json"], "json"));
            ("subtitle-engine transcribe", TRANSCRIBE_USAGE, TRANSCRIBE_HELP)
        }
        "translate" => {
            actions.push(Action { required: true, ..opt(&["--to"], "to") });
            actions.push(opt(&["--from"], "source"));
            actions.push(Action {
                default: || Val::Str("madlad-3b".into(), "madlad-3b".into()),
                ..opt(&["-m", "--model"], "model")
            });
            actions.push(flag(&["--json"], "json"));
            ("subtitle-engine translate", TRANSLATE_USAGE, TRANSLATE_HELP)
        }
        "render" => {
            actions.push(Action {
                choices: Some(FORMATS),
                default: || Val::Str("srt".into(), "srt".into()),
                ..opt(&["-f", "--format"], "format")
            });
            actions.push(opt(&["-o", "--out"], "out"));
            ("subtitle-engine render", RENDER_USAGE, RENDER_HELP)
        }
        "models" => {
            actions.push(flag(&["--json"], "json"));
            ("subtitle-engine models", MODELS_USAGE, MODELS_HELP)
        }
        "delete-model" => {
            actions.push(positional("id"));
            actions.push(flag(&["--json"], "json"));
            ("subtitle-engine delete-model", DELETE_MODEL_USAGE, DELETE_MODEL_HELP)
        }
        other => unreachable!("unknown subcommand {other}"),
    };
    ParserDef { prog, usage, help, actions }
}

// ---------------------------------------------------------------------------------------------------------
// Parsing machinery

#[derive(Clone, Debug)]
struct Arg {
    s: String,
    os: OsString,
}

/// `(action, option_string, sep, explicit_arg)`; `action == None` is an unknown option.
#[derive(Clone, Debug)]
struct OptTuple {
    action: Option<usize>,
    option_string: String,
    sep: Option<String>,
    explicit: Option<String>,
}

type Ns = HashMap<&'static str, Val>;

/// Exit request (help/version/usage error).
struct Exit {
    code: i32,
    stdout: String,
    stderr: String,
}

impl ParserDef {
    fn error(&self, message: &str) -> Exit {
        Exit { code: 2, stdout: String::new(), stderr: format!("{}{}: error: {}\n", self.usage, self.prog, message) }
    }

    /// `ArgumentError(action, message)` turned into `parser.error(str(err))`.
    fn arg_error(&self, action: usize, message: &str) -> Exit {
        self.error(&format!("argument {}: {}", self.actions[action].name(), message))
    }

    /// `_option_string_actions` in insertion order.
    fn option_strings(&self) -> Vec<(&'static str, usize)> {
        let mut out = Vec::new();
        for (i, a) in self.actions.iter().enumerate() {
            for s in a.strings {
                out.push((*s, i));
            }
        }
        out
    }

    fn lookup(&self, option: &str) -> Option<usize> {
        self.option_strings().into_iter().find(|(s, _)| *s == option).map(|(_, i)| i)
    }

    /// `_parse_optional`.
    fn parse_optional(&self, arg: &str) -> Result<Option<OptTuple>, Exit> {
        let mut chars = arg.chars();
        let first = match chars.next() {
            None => return Ok(None),
            Some(c) => c,
        };
        if first != '-' {
            return Ok(None);
        }
        if let Some(action) = self.lookup(arg) {
            return Ok(Some(OptTuple { action: Some(action), option_string: arg.into(), sep: None, explicit: None }));
        }
        if arg.chars().count() == 1 {
            return Ok(None);
        }
        if let Some((option, explicit)) = arg.split_once('=') {
            if let Some(action) = self.lookup(option) {
                return Ok(Some(OptTuple {
                    action: Some(action),
                    option_string: option.into(),
                    sep: Some("=".into()),
                    explicit: Some(explicit.into()),
                }));
            }
        }
        let tuples = self.get_option_tuples(arg);
        if tuples.len() > 1 {
            let options: Vec<&str> = tuples.iter().map(|t| t.option_string.as_str()).collect();
            return Err(self.error(&format!("ambiguous option: {} could match {}", arg, options.join(", "))));
        }
        if let Some(t) = tuples.into_iter().next() {
            return Ok(Some(t));
        }
        // `_negative_number_matcher` = '^-\d+$|^-\d*\.\d+$' (no parser has negative-number-like options).
        if is_negative_number(arg) {
            return Ok(None);
        }
        if arg.contains(' ') {
            return Ok(None);
        }
        Ok(Some(OptTuple { action: None, option_string: arg.into(), sep: None, explicit: None }))
    }

    /// `_get_option_tuples` (allow_abbrev=True).
    fn get_option_tuples(&self, option: &str) -> Vec<OptTuple> {
        let mut result = Vec::new();
        let second = option.chars().nth(1);
        if second == Some('-') {
            let (prefix, sep, explicit) = match option.split_once('=') {
                Some((p, e)) => (p, Some("=".to_string()), Some(e.to_string())),
                None => (option, None, None),
            };
            for (s, action) in self.option_strings() {
                if s.starts_with(prefix) {
                    result.push(OptTuple {
                        action: Some(action),
                        option_string: s.into(),
                        sep: sep.clone(),
                        explicit: explicit.clone(),
                    });
                }
            }
        } else {
            let split = option.char_indices().nth(2).map(|(i, _)| i).unwrap_or(option.len());
            let short_prefix = &option[..split];
            let short_explicit = &option[split..];
            for (s, action) in self.option_strings() {
                if s == short_prefix {
                    result.push(OptTuple {
                        action: Some(action),
                        option_string: s.into(),
                        sep: Some(String::new()),
                        explicit: Some(short_explicit.into()),
                    });
                } else if s.starts_with(option) {
                    result.push(OptTuple { action: Some(action), option_string: s.into(), sep: None, explicit: None });
                }
            }
        }
        result
    }

    /// `_match_argument` for an optional (patterns without `-`): number of args or the nargs error.
    fn match_optional(&self, action: usize, pattern: &[u8]) -> Result<usize, Exit> {
        let leading = pattern.iter().take_while(|&&c| c == b'A').count();
        match self.actions[action].nargs {
            Nargs::Zero => Ok(0),
            Nargs::One if leading >= 1 => Ok(1),
            Nargs::One => Err(self.arg_error(action, "expected one argument")),
            Nargs::OneOrMore if leading >= 1 => Ok(leading),
            Nargs::OneOrMore => Err(self.arg_error(action, "expected at least one argument")),
            Nargs::Parser => unreachable!("PARSER is positional"),
        }
    }

    /// `_match_arguments_partial` for (at most) one positional.
    fn match_positional(&self, action: usize, pattern: &[u8]) -> Option<usize> {
        let dashes = pattern.iter().take_while(|&&c| c == b'-').count();
        if pattern.get(dashes) != Some(&b'A') {
            return None;
        }
        match self.actions[action].nargs {
            // (-*A-*)
            Nargs::One => Some(dashes + 1 + pattern[dashes + 1..].iter().take_while(|&&c| c == b'-').count()),
            // (-*A[-AO]*)
            Nargs::Parser => Some(pattern.len()),
            _ => unreachable!("positionals here take one value or a parser"),
        }
    }

    /// `_get_value` (type conversion).
    fn get_value(&self, action: usize, arg: &Arg) -> Result<Val, Exit> {
        match self.actions[action].ty {
            Ty::Str => Ok(Val::Str(arg.s.clone(), arg.os.clone())),
            Ty::Int => py_int(&arg.s)
                .map(Val::Int)
                .ok_or_else(|| self.arg_error(action, &format!("invalid int value: {}", repr_str(&arg.s)))),
            Ty::Float => py_float(&arg.s)
                .map(Val::Float)
                .ok_or_else(|| self.arg_error(action, &format!("invalid float value: {}", repr_str(&arg.s)))),
        }
    }

    /// `_check_value`.
    fn check_value(&self, action: usize, value: &str) -> Result<(), Exit> {
        if let Some(choices) = self.actions[action].choices {
            if !choices.contains(&value) {
                let listed: Vec<String> = choices.iter().map(|c| repr_str(c)).collect();
                return Err(self.arg_error(
                    action,
                    &format!("invalid choice: {} (choose from {})", repr_str(value), listed.join(", ")),
                ));
            }
        }
        Ok(())
    }

    /// `take_action`: convert the strings and apply the action.
    fn take_action(
        &self,
        action: usize,
        mut strings: Vec<Arg>,
        ns: &mut Ns,
        seen: &mut Vec<usize>,
        unrecognized: &mut Vec<Arg>,
    ) -> Result<(), Exit> {
        seen.push(action);
        let a = &self.actions[action];
        if !a.is_optional() && a.nargs != Nargs::Parser {
            if let Some(i) = strings.iter().position(|s| s.s == "--") {
                strings.remove(i);
            }
        }
        match a.kind {
            Kind::Help => {
                return Err(Exit { code: 0, stdout: self.help.to_string(), stderr: String::new() });
            }
            Kind::Version => {
                return Err(Exit {
                    code: 0,
                    stdout: format!("{} protocol {}\n", self.prog, se_core::events::PROTOCOL_VERSION),
                    stderr: String::new(),
                });
            }
            Kind::StoreTrue => {
                ns.insert(a.dest, Val::Bool(true));
            }
            Kind::Store => {
                let value = if a.nargs == Nargs::OneOrMore {
                    let mut values = Vec::new();
                    for s in &strings {
                        values.push(self.get_value(action, s)?);
                    }
                    for s in &strings {
                        self.check_value(action, &s.s)?;
                    }
                    Val::List(
                        values
                            .into_iter()
                            .map(|v| match v {
                                Val::Str(s, os) => (s, os),
                                _ => unreachable!("lists hold strings"),
                            })
                            .collect(),
                    )
                } else {
                    let value = self.get_value(action, &strings[0])?;
                    self.check_value(action, &strings[0].s)?;
                    value
                };
                ns.insert(a.dest, value);
            }
            Kind::SubParsers => {
                let name = strings[0].s.clone();
                self.check_value(action, &name)?;
                ns.insert(a.dest, Val::Str(name.clone(), strings[0].os.clone()));
                let sub = sub_parser(&name);
                let (sub_ns, extras) = sub.parse_known_args(&strings[1..])?;
                for (k, v) in sub_ns {
                    ns.insert(k, v);
                }
                unrecognized.extend(extras);
            }
        }
        Ok(())
    }

    /// `parse_known_args` → `(namespace, extras)`.
    fn parse_known_args(&self, args: &[Arg]) -> Result<(Ns, Vec<Arg>), Exit> {
        let mut ns: Ns = HashMap::new();
        for a in &self.actions {
            if !matches!(a.kind, Kind::Help | Kind::Version) {
                ns.insert(a.dest, (a.default)());
            }
        }

        // Classification.
        let mut option_indices: HashMap<usize, OptTuple> = HashMap::new();
        let mut pattern: Vec<u8> = Vec::new();
        let mut iter = args.iter().enumerate();
        while let Some((i, arg)) = iter.next() {
            if arg.s == "--" {
                pattern.push(b'-');
                for _ in iter.by_ref() {
                    pattern.push(b'A');
                }
                break;
            }
            match self.parse_optional(&arg.s)? {
                None => pattern.push(b'A'),
                Some(t) => {
                    option_indices.insert(i, t);
                    pattern.push(b'O');
                }
            }
        }

        let mut seen: Vec<usize> = Vec::new();
        let mut unrecognized: Vec<Arg> = Vec::new();
        let mut extras: Vec<Arg> = Vec::new();
        let mut positionals: Vec<usize> =
            (0..self.actions.len()).filter(|&i| !self.actions[i].is_optional()).collect();
        debug_assert!(positionals.len() <= 1);

        let max_option_index = option_indices.keys().copied().max();
        let mut start = 0usize;

        macro_rules! consume_positionals {
            ($start:expr) => {{
                let mut start_index: usize = $start;
                if let Some(&p) = positionals.first() {
                    if let Some(count) = self.match_positional(p, &pattern[start_index..]) {
                        let strings = args[start_index..start_index + count].to_vec();
                        start_index += count;
                        self.take_action(p, strings, &mut ns, &mut seen, &mut unrecognized)?;
                        positionals.remove(0);
                    }
                }
                start_index
            }};
        }

        if let Some(max_index) = max_option_index {
            while start <= max_index {
                let next_option = *option_indices.keys().filter(|&&i| i >= start).min().unwrap();
                if start != next_option {
                    let end = consume_positionals!(start);
                    if end > start {
                        start = end;
                        continue;
                    }
                    start = end;
                }
                if !option_indices.contains_key(&start) {
                    extras.extend_from_slice(&args[start..next_option]);
                    start = next_option;
                }
                start = self.consume_optional(
                    start,
                    args,
                    &pattern,
                    &option_indices,
                    &mut ns,
                    &mut seen,
                    &mut extras,
                    &mut unrecognized,
                )?;
            }
        }
        let stop = consume_positionals!(start);
        extras.extend_from_slice(&args[stop..]);

        let required: Vec<String> = (0..self.actions.len())
            .filter(|i| self.actions[*i].required && !seen.contains(i))
            .map(|i| self.actions[i].name())
            .collect();
        if !required.is_empty() {
            return Err(self.error(&format!("the following arguments are required: {}", required.join(", "))));
        }
        extras.extend(unrecognized);
        Ok((ns, extras))
    }

    #[allow(clippy::too_many_arguments)]
    fn consume_optional(
        &self,
        start_index: usize,
        args: &[Arg],
        pattern: &[u8],
        option_indices: &HashMap<usize, OptTuple>,
        ns: &mut Ns,
        seen: &mut Vec<usize>,
        extras: &mut Vec<Arg>,
        unrecognized: &mut Vec<Arg>,
    ) -> Result<usize, Exit> {
        let t = option_indices[&start_index].clone();
        let mut action = t.action;
        let mut option_string = t.option_string;
        let mut sep = t.sep;
        let mut explicit = t.explicit;
        let mut tuples: Vec<(usize, Vec<Arg>)> = Vec::new();
        let stop;
        loop {
            let Some(a) = action else {
                extras.push(args[start_index].clone());
                return Ok(start_index + 1);
            };
            if let Some(exp) = explicit.clone() {
                let count = self.match_optional(a, b"A")?;
                let second = option_string.chars().nth(1);
                if count == 0 && second != Some('-') && !exp.is_empty() {
                    if sep.as_deref().is_some_and(|s| !s.is_empty()) || exp.starts_with('-') {
                        return Err(self.arg_error(a, &format!("ignored explicit argument {}", repr_str(&exp))));
                    }
                    tuples.push((a, Vec::new()));
                    let mut chars = exp.chars();
                    let c = chars.next().unwrap();
                    let rest: String = chars.collect();
                    option_string = format!("-{c}");
                    if let Some(next) = self.lookup(&option_string) {
                        action = Some(next);
                        if rest.is_empty() {
                            sep = None;
                            explicit = None;
                        } else if let Some(r) = rest.strip_prefix('=') {
                            sep = Some("=".into());
                            explicit = Some(r.to_string());
                        } else {
                            sep = Some(String::new());
                            explicit = Some(rest);
                        }
                    } else {
                        let s = format!("-{exp}");
                        extras.push(Arg { os: OsString::from(&s), s });
                        stop = start_index + 1;
                        break;
                    }
                } else if count == 1 {
                    stop = start_index + 1;
                    tuples.push((a, vec![Arg { os: OsString::from(&exp), s: exp }]));
                    break;
                } else {
                    return Err(self.arg_error(a, &format!("ignored explicit argument {}", repr_str(&exp))));
                }
            } else {
                let begin = start_index + 1;
                let count = self.match_optional(a, &pattern[begin..])?;
                stop = begin + count;
                tuples.push((a, args[begin..stop].to_vec()));
                break;
            }
        }
        for (a, strings) in tuples {
            self.take_action(a, strings, ns, seen, unrecognized)?;
        }
        Ok(stop)
    }
}

fn is_negative_number(arg: &str) -> bool {
    let Some(rest) = arg.strip_prefix('-') else { return false };
    let digit = |c: char| c.is_ascii_digit() || crate_decimal(c);
    if !rest.is_empty() && rest.chars().all(digit) {
        return true;
    }
    match rest.split_once('.') {
        Some((a, b)) => a.chars().all(digit) && !b.is_empty() && b.chars().all(digit),
        None => false,
    }
}

/// Python `re` `\d` matches every Unicode decimal digit.
fn crate_decimal(c: char) -> bool {
    !c.is_ascii() && crate::pyparse::py_int(&c.to_string()).is_some()
}

// ---------------------------------------------------------------------------------------------------------
// Public entry

fn to_arg(os: &OsStr) -> Arg {
    Arg { s: se_core::paths::os_to_py_string(os), os: os.to_os_string() }
}

fn take_str(ns: &mut Ns, dest: &str) -> Option<String> {
    match ns.remove(dest) {
        Some(Val::Str(s, _)) => Some(s),
        _ => None,
    }
}

fn take_path(ns: &mut Ns, dest: &str) -> Option<PathBuf> {
    match ns.remove(dest) {
        Some(Val::Str(_, os)) => Some(PathBuf::from(os)),
        _ => None,
    }
}

fn take_bool(ns: &mut Ns, dest: &str) -> bool {
    matches!(ns.remove(dest), Some(Val::Bool(true)))
}

fn take_int(ns: &mut Ns, dest: &str) -> Option<i64> {
    match ns.remove(dest) {
        Some(Val::Int(v)) => Some(v),
        _ => None,
    }
}

/// Parse `argv` (without the program name).
pub fn parse(argv: &[OsString]) -> Parsed {
    let top = top_parser();
    let args: Vec<Arg> = argv.iter().map(|a| to_arg(a)).collect();
    let (mut ns, extras) = match top.parse_known_args(&args) {
        Ok(v) => v,
        Err(e) => return Parsed::Exit { code: e.code, stdout: e.stdout, stderr: e.stderr },
    };
    if !extras.is_empty() {
        let joined: Vec<&str> = extras.iter().map(|a| a.s.as_str()).collect();
        let e = top.error(&format!("unrecognized arguments: {}", joined.join(" ")));
        return Parsed::Exit { code: e.code, stdout: e.stdout, stderr: e.stderr };
    }
    let name = take_str(&mut ns, "command").expect("required command");
    let json = take_bool(&mut ns, "json");
    let command = match name.as_str() {
        "extract-audio" => Command::ExtractAudio {
            video: take_path(&mut ns, "video").unwrap(),
            out_dir: take_path(&mut ns, "out_dir").unwrap(),
            json,
        },
        "preview" => Command::Preview {
            video: take_path(&mut ns, "video").unwrap(),
            output: take_path(&mut ns, "output").unwrap(),
            json,
        },
        "transcribe" => Command::Transcribe {
            video: take_path(&mut ns, "video").unwrap(),
            out_dir: take_path(&mut ns, "out_dir").unwrap(),
            language: take_str(&mut ns, "language"),
            model: take_str(&mut ns, "model").unwrap(),
            formats: match ns.remove("format") {
                Some(Val::List(v)) => v.into_iter().map(|(s, _)| s).collect(),
                _ => unreachable!("format is a list"),
            },
            max_chars: take_int(&mut ns, "max_chars").unwrap(),
            max_duration: match ns.remove("max_duration") {
                Some(Val::Float(v)) => v,
                _ => unreachable!("max_duration is a float"),
            },
            diarize: take_bool(&mut ns, "diarize"),
            speakers: take_int(&mut ns, "speakers"),
            json,
        },
        "translate" => Command::Translate {
            to: take_str(&mut ns, "to").unwrap(),
            source: take_str(&mut ns, "source"),
            model: take_str(&mut ns, "model").unwrap(),
            json,
        },
        "render" => Command::Render { format: take_str(&mut ns, "format").unwrap(), out: take_path(&mut ns, "out") },
        "models" => Command::Models { json },
        "delete-model" => Command::DeleteModel { id: take_str(&mut ns, "id").unwrap(), json },
        _ => unreachable!("choices checked"),
    };
    Parsed::Run(Args {
        lang: take_str(&mut ns, "lang"),
        sub_lang: take_str(&mut ns, "sub_lang"),
        offline: take_bool(&mut ns, "offline"),
        command,
    })
}
