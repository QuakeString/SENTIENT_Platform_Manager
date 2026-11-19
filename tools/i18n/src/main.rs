// Copyright © 2016-2026 The SENTIENT Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The Platform Manager's text tool.
//!
//!   spm-i18n-tool scan <src> <out.tsv>       every string literal that may be on screen
//!   spm-i18n-tool apply <src> <in.tsv>       wrap the rows marked `yes` in t!/tf!
//!   spm-i18n-tool collect <src> <en.json>    the catalogue: every t!/tf! text
//!
//! `scan` writes one row per literal: decision (yes, maybe, const, no),
//! reason, file, line, column, kind (t or tf) and the text (escaped). A person
//! (or a review pass) turns `maybe` into `yes` or `no`; `apply` then rewrites
//! the files from the bottom up so the offsets stay right. Run `apply` on a
//! clean tree: the rows point at the tree `scan` read.

use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Clone, Default)]
struct Ctx {
    in_attr: bool,
    in_test: bool,
    in_const: bool,
    /// The call the group belongs to: `.child`, `format!`, `Command::new`.
    callee: Option<String>,
    /// The macro or call the enclosing group's parent belongs to (`.children([..])`).
    outer: Option<String>,
}

struct Found {
    file: PathBuf,
    line: usize,
    col: usize,
    decision: &'static str,
    reason: String,
    kind: &'static str,
    text: String,
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut v: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    v.sort();
    for p in v {
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

fn is_punct(t: Option<&TokenTree>, c: char) -> bool {
    matches!(t, Some(TokenTree::Punct(p)) if p.as_char() == c)
}

fn ident(t: Option<&TokenTree>) -> Option<String> {
    match t {
        Some(TokenTree::Ident(i)) => Some(i.to_string()),
        _ => None,
    }
}

/// The name of the call a group at `i` belongs to, from the tokens before it.
fn callee_of(tokens: &[TokenTree], i: usize) -> Option<String> {
    if i == 0 {
        return None;
    }
    let prev = tokens.get(i - 1);
    // macro!( .. )
    if is_punct(prev, '!') {
        return ident(tokens.get(i.wrapping_sub(2))).map(|n| format!("{n}!"));
    }
    // generic turbofish  name::<T>( ..) is rare; ignore.
    let name = ident(prev)?;
    if i >= 2 && is_punct(tokens.get(i - 2), '.') {
        return Some(format!(".{name}"));
    }
    // Path::name(
    if i >= 3 && is_punct(tokens.get(i - 2), ':') && is_punct(tokens.get(i - 3), ':') {
        if let Some(head) = ident(tokens.get(i.wrapping_sub(4))) {
            return Some(format!("{head}::{name}"));
        }
    }
    Some(name)
}

/// Literal string value and whether it is a plain (non-byte, non-C) string.
fn string_value(lit: &proc_macro2::Literal) -> Option<String> {
    let s = lit.to_string();
    if !(s.starts_with('"') || s.starts_with("r\"") || s.starts_with("r#")) {
        return None;
    }
    syn::parse_str::<syn::LitStr>(&s).ok().map(|l| l.value())
}

/// Format placeholders other than `{{`/`}}`.
fn has_placeholder(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            if b.get(i + 1) == Some(&b'{') {
                i += 2;
                continue;
            }
            return true;
        }
        i += 1;
    }
    false
}

/// Text with the placeholders taken out, for the letter checks.
fn without_placeholders(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn has_word(s: &str) -> bool {
    let mut run = 0;
    for c in s.chars() {
        if c.is_alphabetic() {
            run += 1;
            if run >= 2 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Looks like code, a path, a command or an identifier, never on screen as a sentence.
fn looks_like_code(s: &str) -> Option<&'static str> {
    let t = s.trim();
    let plain = without_placeholders(t);
    if !has_word(&plain) {
        return Some("no-words");
    }
    let spaced = plain.contains(' ');
    if t.starts_with("http://") || t.starts_with("https://") || t.contains("://") && !spaced {
        return Some("url");
    }
    let shell = ["$(", "&&", "||", "2>", ">/dev/null", "| ", "sudo ", "systemctl ", "docker ", "wsl ", "powershell", "Get-", "Set-", "New-", "$env:", "${", "#!/", "\\\\", "HKEY_", "netsh ", "sc.exe", "icacls", "chmod ", "chown ", "mkdir ", "rm -", "grep ", "sed ", "awk ", "printf ", "echo ", "export ", "cat ", "tar ", "psql", "pg_", "SELECT ", "INSERT ", "UPDATE ", "DELETE ", "CREATE ", "ALTER ", "DROP ", " FROM ", " WHERE ", "--", "-c ", "%PROGRAM", "%APPDATA", "C:\\", "/etc/", "/var/", "/usr/", "/opt/", "/home/", "/tmp/", "~/", "Remove-Item", "-LiteralPath", "-Path ", "-Force", "ssh-keygen", "\"{", "'{", "[Unit]", "[Service]", "[Install]", "ExecStart", "Description=", "reput ", "mkdir", "cmd /c", "cmd.exe", "reg ", "schtasks", "/v ", "/d ", "Start-", "Stop-", "Invoke-", "-ErrorAction", "Out-Null", "Test-Path", "$_", "2>nul", " >nul", "%~", "pkexec", "runas", "ProgramData", "LOCALAPPDATA", "AppData"];
    for p in shell {
        // a word ("sed ", "cat ") only where it starts a word: not "Compressed "
        let word = p.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
        let hit = t.match_indices(p).any(|(i, _)| !word || i == 0 || !t[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric()));
        if hit {
            return Some("shell-or-path");
        }
    }
    if t.starts_with('-') || t.starts_with('/') || t.starts_with('.') && !t.starts_with("...") || t.starts_with('<') && t.ends_with('>') {
        return Some("flag-or-path");
    }
    if !spaced {
        // ids, keys, env names, file names, versions
        if plain.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_.:/=@+".contains(c)) {
            return Some("id-like");
        }
        if plain.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || "_-.".contains(c)) {
            return Some("CONST-like");
        }
        if plain.contains('_') || plain.contains('/') || plain.contains('\\') || plain.contains('=') || plain.contains("::") {
            return Some("code-ish");
        }
        if plain.contains('.') && !plain.ends_with('.') {
            return Some("file-or-version");
        }
        // NoRepair, SentientGateway, PostgreSQL: names, not words
        if plain.chars().skip(1).any(|c| c.is_uppercase()) {
            return Some("camel-or-name");
        }
    }
    if t.starts_with('{') && t.ends_with('}') && t.contains('"') {
        return Some("json");
    }
    None
}

const UI_CALLEES: &[&str] = &[
    ".child", ".label", ".tooltip", "tooltip", ".placeholder", ".title", ".description", "show_info", "show_error", "show_message", "pill", ".prefix_text", ".suffix_text", ".message",
    ".context", ".with_context", ".expect_ui", "info", "warning", "note", "heading", "section_title", "row", "field_row", "card", ".text", "Some", "confirm", ".header",
    "page_header", "section", "SharedString::from", "Err", "Ok", "Label::new", "Checkbox::new", "Text::new", "ui::info",
];

const LOGIC_CALLEES: &[&str] = &[
    "button", ".id", "id", ".arg", ".args", "Command::new", "var", "var_os", "env::var", "env::var_os", "std::env::var", "set_var", ".join", ".starts_with", ".ends_with",
    ".contains", ".strip_prefix", ".strip_suffix", ".split", ".splitn", ".rsplit", ".rsplitn", ".split_once", ".rsplit_once", ".replace", ".replacen", ".find", ".rfind", ".trim_start_matches",
    ".trim_end_matches", ".trim_matches", ".get", ".get_mut", ".remove", ".entry", ".contains_key", "parse_from_str", ".format", ".parse", ".key", ".eq", ".query", ".execute", ".prepare",
    ".query_one", ".query_opt", ".batch_execute", ".simple_query", "Path::new", "PathBuf::from", ".push", ".with_extension", ".set_extension", ".file_name", ".with_file_name", ".matches",
    ".write_all", ".header_value", ".env", ".current_dir", ".lines", "which", "which::which", ".pointer", ".write", ".to_socket_addrs", "OsStr::new", "OsString::from", ".exec",
    "layout_probe", "probe", "probed_as", "label_probe", "Icon::new", "icon", "svg", ".path", "IconName", "load_svg", "asset", "Asset::get", "include_svg", "debug_flag", "debug_env",
];

const SKIP_MACROS: &[&str] = &[
    "info!", "warn!", "error!", "debug!", "trace!", "log!", "println!", "eprintln!", "print!", "eprint!", "assert!", "assert_eq!", "assert_ne!", "debug_assert!", "debug_assert_eq!",
    "panic!", "unreachable!", "todo!", "unimplemented!", "include_str!", "include_bytes!", "concat!", "env!", "option_env!", "matches!", "cfg!", "json!", "stringify!", "compile_error!",
    "event!", "span!", "info_span!", "debug_span!", "log_line!", "vec_skip!", "dbg!", "expect!", "trace_step!",
];

/// Macros whose first literal is a format string shown to a person.
const FORMAT_MACROS: &[&str] = &["format!", "anyhow!", "bail!", "ensure!", "write!", "writeln!", "format_args!"];

fn decide(text: &str, ctx: &Ctx, prev: &[Option<&TokenTree>; 3], next: Option<&TokenTree>, arg_index: usize) -> (&'static str, String) {
    if ctx.in_attr {
        return ("no", "attribute".into());
    }
    if ctx.in_test {
        return ("no", "test".into());
    }
    let callee = ctx.callee.clone().unwrap_or_default();
    if matches!(callee.as_str(), "t!" | "tf!" | "n!" | "nf!") {
        return ("no", "wrapped".into());
    }
    if SKIP_MACROS.contains(&callee.as_str()) {
        return ("no", format!("macro {callee}"));
    }
    if let Some(why) = looks_like_code(text) {
        return ("no", why.into());
    }
    // comparisons and match patterns
    if is_punct(prev[0], '=') && is_punct(prev[1], '=') || is_punct(prev[0], '=') && is_punct(prev[1], '!') {
        return ("no", "compared".into());
    }
    if is_punct(next, '=') {
        if let Some(TokenTree::Punct(p)) = next {
            if p.spacing() == Spacing::Joint {
                return ("no", "pattern-or-compare".into());
            }
        }
    }
    if is_punct(next, '|') || is_punct(prev[0], '|') && !is_punct(prev[1], '|') && ctx.callee.as_deref() != Some("") {
        // match arm alternatives "a" | "b" =>; closures |x| "a" are rare with literals
        if is_punct(next, '|') {
            return ("no", "pattern".into());
        }
    }
    if ctx.in_const {
        return ("const", "const-or-static".into());
    }
    if LOGIC_CALLEES.contains(&callee.as_str()) {
        // button(id) / .id(..) only for their first argument
        return ("no", format!("call {callee}"));
    }
    if FORMAT_MACROS.contains(&callee.as_str()) {
        let first = if callee == "write!" || callee == "writeln!" || callee == "ensure!" { 1 } else { 0 };
        if arg_index != first {
            return ("no", format!("{callee} arg {arg_index}"));
        }
        return ("maybe", format!("fmt {callee}"));
    }
    // field: label: "x"
    if is_punct(prev[0], ':') && !is_punct(prev[1], ':') {
        if let Some(TokenTree::Ident(f)) = prev[1] {
            let f = f.to_string();
            let ui_fields = ["label", "title", "text", "tip", "tooltip", "message", "description", "hint", "help", "summary", "detail", "details", "note", "heading", "caption", "subtitle", "body", "what", "why", "fix", "action_label", "secondary", "name", "step", "headline", "explain", "reason"];
            if ui_fields.contains(&f.as_str()) {
                return ("yes", format!("field {f}"));
            }
            return ("maybe", format!("field {f}"));
        }
    }
    if UI_CALLEES.contains(&callee.as_str()) {
        return ("yes", format!("call {callee}"));
    }
    if callee.is_empty() {
        if let Some(o) = &ctx.outer {
            if UI_CALLEES.contains(&o.as_str()) || o == ".children" {
                return ("yes", format!("in {o}"));
            }
        }
    }
    let plain = without_placeholders(text);
    if plain.contains(' ') || plain.chars().next().is_some_and(|c| c.is_uppercase()) {
        return ("maybe", if callee.is_empty() { "bare".into() } else { format!("call {callee}") });
    }
    ("no", format!("word {callee}"))
}

fn walk(file: &Path, src_lines: &[&str], ts: TokenStream, ctx: &Ctx, out: &mut Vec<Found>) {
    let tokens: Vec<TokenTree> = ts.into_iter().collect();
    let mut pending_test = false;
    let mut const_until_semi = false;
    let mut arg_index = 0usize;
    for i in 0..tokens.len() {
        let t = &tokens[i];
        match t {
            TokenTree::Ident(id) => {
                let s = id.to_string();
                if (s == "const" || s == "static") && !matches!(ident(tokens.get(i + 1)).as_deref(), Some("fn") | Some("move") | Some("unsafe")) && !is_punct(tokens.get(i.wrapping_sub(1)), '&') {
                    // `static` in `&'static str` is a lifetime ident after a quote punct
                    if !is_punct(tokens.get(i.wrapping_sub(1)), '\'') {
                        const_until_semi = true;
                    }
                }
            }
            TokenTree::Punct(p) => {
                if p.as_char() == ';' {
                    const_until_semi = false;
                    pending_test = false;
                }
                if p.as_char() == ',' {
                    arg_index += 1;
                }
            }
            TokenTree::Group(g) => {
                let is_attr = is_punct(tokens.get(i.wrapping_sub(1)), '#') || (is_punct(tokens.get(i.wrapping_sub(1)), '!') && is_punct(tokens.get(i.wrapping_sub(2)), '#'));
                let mut c = ctx.clone();
                if is_attr {
                    c.in_attr = true;
                    let s = g.stream().to_string();
                    if s.starts_with("cfg") && !s.starts_with("cfg_attr") && s.contains("test") && !s.contains("not (test)") && !s.contains("not(test)") {
                        pending_test = true;
                    }
                } else if g.delimiter() == Delimiter::Brace && pending_test {
                    c.in_test = true;
                    pending_test = false;
                }
                if const_until_semi {
                    c.in_const = true;
                }
                // a const fn / static initialiser body is not a const context once in a fn body
                if g.delimiter() == Delimiter::Brace && !const_until_semi {
                    c.in_const = false;
                }
                c.outer = ctx.callee.clone();
                c.callee = callee_of(&tokens, i);
                if g.delimiter() == Delimiter::Brace {
                    c.callee = None;
                    c.outer = None;
                }
                walk(file, src_lines, g.stream(), &c, out);
            }
            TokenTree::Literal(l) => {
                let Some(text) = string_value(l) else { continue };
                let prev = [tokens.get(i.wrapping_sub(1)), tokens.get(i.wrapping_sub(2)), tokens.get(i.wrapping_sub(3))];
                let next = tokens.get(i + 1);
                let (decision, reason) = decide(&text, ctx, &prev, next, arg_index);
                let fmt = ctx.callee.as_deref().is_some_and(|c| FORMAT_MACROS.contains(&c));
                let kind = if fmt && has_placeholder(&text) { "tf" } else { "t" };
                let start = l.span().start();
                let _ = src_lines;
                out.push(Found { file: file.to_path_buf(), line: start.line, col: start.column, decision, reason, kind, text });
            }
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "\\r")
}

fn unesc(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn scan(src: &Path, out_path: &Path) {
    let mut files = Vec::new();
    rs_files(src, &mut files);
    let mut all = Vec::new();
    for f in &files {
        let code = std::fs::read_to_string(f).unwrap();
        let lines: Vec<&str> = code.lines().collect();
        let ts = TokenStream::from_str(&code).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        walk(f, &lines, ts, &Ctx::default(), &mut all);
    }
    let mut s = String::from("decision\treason\tfile\tline\tcol\tkind\ttext\n");
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &all {
        *counts.entry(f.decision).or_default() += 1;
        let rel = f.file.strip_prefix(src).unwrap_or(&f.file);
        s.push_str(&format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\n", f.decision, f.reason, rel.display(), f.line, f.col, f.kind, esc(&f.text)));
    }
    std::fs::write(out_path, s).unwrap();
    eprintln!("{} literals in {} files: {:?}", all.len(), files.len(), counts);
}

/// Byte offset of a (1-based line, char column) position.
fn offset(code: &str, line: usize, col: usize) -> usize {
    let mut off = 0;
    for (n, l) in code.split_inclusive('\n').enumerate() {
        if n + 1 == line {
            return off + l.char_indices().nth(col).map(|(b, _)| b).unwrap_or(l.len());
        }
        off += l.len();
    }
    code.len()
}

/// Rewrites one file: each chosen literal at (line, col).
fn apply_file(path: &Path, rows: &[(usize, usize, String)]) -> usize {
    let mut code = std::fs::read_to_string(path).unwrap();
    // find every literal and its enclosing macro call with real spans
    let ts = TokenStream::from_str(&code).unwrap();
    let mut edits: Vec<(usize, usize, String)> = Vec::new(); // byte start, byte end, replacement
    let wanted: std::collections::HashMap<(usize, usize), String> = rows.iter().map(|(l, c, k)| ((*l, *c), k.clone())).collect();
    collect_edits(&code, ts, None, &wanted, &mut edits);
    edits.sort_by(|a, b| b.0.cmp(&a.0));
    let n = edits.len();
    for (s, e, r) in edits {
        code.replace_range(s..e, &r);
    }
    std::fs::write(path, code).unwrap();
    n
}

/// One edit per wanted literal. Inside format!: the macro name becomes tf
/// (format!) or the whole argument list is wrapped (anyhow!, bail!, write!).
fn collect_edits(code: &str, ts: TokenStream, parent: Option<(&[TokenTree], usize)>, wanted: &std::collections::HashMap<(usize, usize), String>, edits: &mut Vec<(usize, usize, String)>) {
    let tokens: Vec<TokenTree> = ts.into_iter().collect();
    for (i, t) in tokens.iter().enumerate() {
        match t {
            TokenTree::Group(g) => collect_edits(code, g.stream(), Some((&tokens, i)), wanted, edits),
            TokenTree::Literal(l) => {
                let st = l.span().start();
                let Some(kind) = wanted.get(&(st.line, st.column)) else { continue };
                let en = l.span().end();
                let s = offset(code, st.line, st.column);
                let e = offset(code, en.line, en.column);
                let lit = &code[s..e];
                let mac = parent.and_then(|(pt, gi)| callee_of(pt, gi).map(|c| (c, pt, gi)));
                if kind == "n" {
                    edits.push((s, e, format!("n!({lit})")));
                    continue;
                }
                match mac {
                    Some((c, pt, gi)) if FORMAT_MACROS.contains(&c.as_str()) && kind == "tf" => {
                        if c == "format!" {
                            // the ident `format` before `!`
                            if let TokenTree::Ident(id) = &pt[gi - 2] {
                                let a = offset(code, id.span().start().line, id.span().start().column);
                                let b = offset(code, id.span().end().line, id.span().end().column);
                                edits.push((a, b, "tf".into()));
                            }
                        } else if let TokenTree::Group(g) = &pt[gi] {
                            // wrap from the literal to the group's end
                            let close = g.span_close().start();
                            let gend = offset(code, close.line, close.column);
                            let inner = code[s..gend].trim_end().trim_end_matches(',').to_string();
                            let a = s;
                            let b = s + code[s..gend].trim_end().trim_end_matches(',').len();
                            let _ = inner;
                            edits.push((a, b, format!("{{prefix}}tf!({})", &code[a..b])));
                        }
                    }
                    Some((c, _, _)) if FORMAT_MACROS.contains(&c.as_str()) => {
                        if c == "format!" {
                            // format!("literal") with no placeholders: t!(..).to_string()
                            edits.push((s, e, format!("\"{{}}\", t!({lit})")));
                        } else {
                            edits.push((s, e, format!("t!({lit})")));
                        }
                    }
                    _ => edits.push((s, e, format!("t!({lit})"))),
                }
            }
            _ => {}
        }
    }
    // write!(f, "{}", tf!(..)) needs the "{}", before; anyhow!(tf!(..)) does not
    for e in edits.iter_mut() {
        if e.2.starts_with("{prefix}") {
            let rest = e.2.trim_start_matches("{prefix}").to_string();
            let before = &code[..e.0];
            let is_write = before.trim_end().trim_end_matches(|c: char| c != '(').trim_end_matches('(').trim_end().ends_with("write!") || before.trim_end().contains("writeln!(") && before.rfind("writeln!(").is_some_and(|p| !before[p..].contains(';'));
            e.2 = if is_write { format!("\"{{}}\", {rest}") } else { rest };
        }
    }
}

fn apply(src: &Path, tsv: &Path) {
    let data = std::fs::read_to_string(tsv).unwrap();
    let mut by_file: BTreeMap<String, Vec<(usize, usize, String)>> = BTreeMap::new();
    for row in data.lines().skip(1) {
        let c: Vec<&str> = row.splitn(7, '\t').collect();
        if c.len() < 7 || !(c[0] == "yes" || c[0] == "const") {
            continue;
        }
        let kind = if c[0] == "const" { "n" } else { c[5] };
        by_file.entry(c[2].to_string()).or_default().push((c[3].parse().unwrap(), c[4].parse().unwrap(), kind.to_string()));
    }
    let mut total = 0;
    for (f, rows) in by_file {
        total += apply_file(&src.join(&f), &rows);
    }
    eprintln!("{total} texts wrapped");
}

/// Every literal given to t!/tf!, in file order, once.
fn collect(src: &Path, out_path: &Path) {
    let mut files = Vec::new();
    rs_files(src, &mut files);
    let mut seen = serde_json::Map::new();
    for f in &files {
        let code = std::fs::read_to_string(f).unwrap();
        let ts = TokenStream::from_str(&code).unwrap();
        collect_t(ts, &mut seen);
    }
    // The settings catalogue's names, labels and help (crate::catalog).
    let cat = src.join("../assets/settings_catalog.json");
    if let Ok(t) = std::fs::read_to_string(&cat) {
        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
        for g in v["groups"].as_array().into_iter().flatten() {
            let mut add = |s: &serde_json::Value| {
                if let Some(s) = s.as_str().filter(|s| !s.is_empty()) {
                    seen.entry(s.to_string()).or_insert(serde_json::Value::String(String::new()));
                }
            };
            add(&g["name"]);
            for s in g["settings"].as_array().into_iter().flatten() {
                add(&s["label"]);
                add(&s["help"]);
            }
        }
    }
    eprintln!("{} texts", seen.len());
    std::fs::write(out_path, serde_json::to_string_pretty(&serde_json::Value::Object(seen)).unwrap() + "\n").unwrap();
}

fn collect_t(ts: TokenStream, seen: &mut serde_json::Map<String, serde_json::Value>) {
    let tokens: Vec<TokenTree> = ts.into_iter().collect();
    for (i, t) in tokens.iter().enumerate() {
        if let TokenTree::Group(g) = t {
            let c = callee_of(&tokens, i);
            if matches!(c.as_deref(), Some("t!") | Some("tf!") | Some("n!") | Some("nf!")) {
                if let Some(TokenTree::Literal(l)) = g.stream().into_iter().next() {
                    if let Some(v) = string_value(&l) {
                        let v = if matches!(c.as_deref(), Some("tf!") | Some("nf!")) { normalize(&v) } else { v };
                        seen.entry(v).or_insert(serde_json::Value::String(String::new()));
                    }
                }
            }
            collect_t(g.stream(), seen);
        }
    }
}

/// tf!'s key: placeholders as {0}, {1} or {name}, without format specs
/// (as spm-i18n-macros writes it).
fn normalize(text: &str) -> String {
    let mut key = String::new();
    let mut next = 0usize;
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '{' if it.peek() == Some(&'{') => {
                it.next();
                key.push_str("{{");
            }
            '}' if it.peek() == Some(&'}') => {
                it.next();
                key.push_str("}}");
            }
            '{' => {
                let mut inner = String::new();
                for c in it.by_ref() {
                    if c == '}' {
                        break;
                    }
                    inner.push(c);
                }
                let arg = inner.split(':').next().unwrap_or("").trim().to_string();
                if arg.is_empty() {
                    key.push_str(&format!("{{{next}}}"));
                    next += 1;
                } else {
                    key.push_str(&format!("{{{arg}}}"));
                }
            }
            c => key.push(c),
        }
    }
    key
}

/// Job data: the texts that name a job's steps and messages are matched
/// between the plan and the work (and come from servers), so they stay
/// English (n!, nf!) and the job view translates them as it shows them
/// (crate::i18n::dynamic).
const DATA_CALLEES: &[&str] = &["PlanStep::new", "Progress::Step", "Progress::Done", "Progress::Error", "Progress::Log", ".detail", "PlanStep"];

/// Errors are compared by text in many places (`e == remote::AGENT_OLD`,
/// `e.contains("administrator rights")`), so they stay English too and are
/// translated where shown.
const ERROR_CALLEES: &[&str] = &["Err", "anyhow!", "bail!", "ensure!", ".context", ".with_context", "failed", "remote::failed", ".ok_or", ".ok_or_else", ".map_err", "Error::msg", "anyhow::anyhow!", "anyhow::bail!"];

fn demote(src: &Path, whole: &[String]) {
    let mut files = Vec::new();
    rs_files(src, &mut files);
    let mut total = 0;
    for f in files {
        let rel = f.strip_prefix(src).unwrap().display().to_string();
        let mut code = std::fs::read_to_string(&f).unwrap();
        let ts = TokenStream::from_str(&code).unwrap();
        let mut edits = Vec::new();
        demote_walk(&code, ts, whole.contains(&rel), &mut edits);
        if edits.is_empty() {
            continue;
        }
        edits.sort_by(|a: &(usize, usize, &str), b| b.0.cmp(&a.0));
        edits.dedup();
        total += edits.len();
        for (a, b, r) in edits {
            code.replace_range(a..b, r);
        }
        std::fs::write(&f, code).unwrap();
    }
    eprintln!("{total} texts made job data");
}

fn demote_walk(code: &str, ts: TokenStream, data: bool, edits: &mut Vec<(usize, usize, &'static str)>) {
    let tokens: Vec<TokenTree> = ts.into_iter().collect();
    for (i, t) in tokens.iter().enumerate() {
        if let TokenTree::Group(g) = t {
            let c = callee_of(&tokens, i);
            let first_amp = matches!(g.stream().into_iter().next(), Some(TokenTree::Punct(p)) if p.as_char() == '&');
            let errors = std::env::var_os("DEMOTE_ERRORS").is_some();
            let inner = data
                || c.as_deref().is_some_and(|c| DATA_CALLEES.contains(&c) || errors && ERROR_CALLEES.contains(&c))
                || (c.as_deref() == Some(".when") && first_amp);
            if inner && matches!(c.as_deref(), Some("t!") | Some("tf!")) {
                if let TokenTree::Ident(id) = &tokens[i - 2] {
                    let a = offset(code, id.span().start().line, id.span().start().column);
                    let b = offset(code, id.span().end().line, id.span().end().column);
                    edits.push((a, b, if id == "t" { "n" } else { "nf" }));
                }
            }
            demote_walk(code, g.stream(), inner, edits);
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("scan") => scan(Path::new(&a[2]), Path::new(&a[3])),
        Some("apply") => apply(Path::new(&a[2]), Path::new(&a[3])),
        Some("collect") => collect(Path::new(&a[2]), Path::new(&a[3])),
        Some("demote") => demote(Path::new(&a[2]), &a[3..]),
        _ => {
            eprintln!("usage: spm-i18n-tool scan|apply|collect <src> <file>");
            std::process::exit(2);
        }
    }
    let _ = unesc;
}
