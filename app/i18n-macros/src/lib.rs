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

//! The Platform Manager's text macros.
//!
//! `t!("Back up now")` is a `&'static str` in the chosen language.
//!
//! `tf!("Checked {}: {name}", when, name = n)` takes what `format!` takes
//! (positional, `name = expr` and names caught from the scope, with format
//! specs) and is a `String`. Its catalogue key is the text with every
//! placeholder written as `{0}`, `{1}` .. or `{name}` and no format spec
//! (`{:.1} GB` is `{0} GB`), so a translation can move them. Each argument is
//! formatted with its own spec before it goes in.

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Expr, Ident, LitStr, Token};

struct Arg {
    name: Option<Ident>,
    expr: Expr,
}

impl Parse for Arg {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek(Ident) && input.peek2(Token![=]) && !input.peek2(Token![==]) {
            let name: Ident = input.parse()?;
            let _: Token![=] = input.parse()?;
            return Ok(Arg { name: Some(name), expr: input.parse()? });
        }
        Ok(Arg { name: None, expr: input.parse()? })
    }
}

struct Input {
    text: LitStr,
    args: Vec<Arg>,
}

impl Parse for Input {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let text: LitStr = input.parse()?;
        let mut args = Vec::new();
        if input.peek(Token![,]) {
            let _: Token![,] = input.parse()?;
            let list: Punctuated<Arg, Token![,]> = Punctuated::parse_terminated(input)?;
            args = list.into_iter().collect();
        }
        Ok(Input { text, args })
    }
}

/// A placeholder as written: which argument, and its format spec.
enum Which {
    Index(usize),
    Name(String),
}

/// The key (placeholders as `{0}`/`{name}`) and the placeholders in order.
fn normalize(text: &str) -> Result<(String, Vec<(Which, String)>), String> {
    let mut key = String::new();
    let mut found = Vec::new();
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
                loop {
                    match it.next() {
                        Some('}') => break,
                        Some(c) => inner.push(c),
                        None => return Err(format!("unclosed {{ in {text:?}")),
                    }
                }
                let (arg, spec) = match inner.split_once(':') {
                    Some((a, s)) => (a.trim().to_string(), s.to_string()),
                    None => (inner.trim().to_string(), String::new()),
                };
                let which = if arg.is_empty() {
                    next += 1;
                    Which::Index(next - 1)
                } else if let Ok(n) = arg.parse::<usize>() {
                    Which::Index(n)
                } else {
                    Which::Name(arg)
                };
                match &which {
                    Which::Index(n) => key.push_str(&format!("{{{n}}}")),
                    Which::Name(n) => key.push_str(&format!("{{{n}}}")),
                }
                found.push((which, spec));
            }
            '}' => return Err(format!("unmatched }} in {text:?}")),
            c => key.push(c),
        }
    }
    Ok((key, found))
}

/// `t!("text")`: the text in the chosen language, a `&'static str`.
#[proc_macro]
pub fn t(input: TokenStream) -> TokenStream {
    let text = syn::parse_macro_input!(input as LitStr);
    quote!(crate::i18n::t(#text)).into()
}

/// `tf!("text {}", args..)`: as `format!`, in the chosen language.
#[proc_macro]
pub fn tf(input: TokenStream) -> TokenStream {
    let Input { text, args } = syn::parse_macro_input!(input as Input);
    let (key, found) = match normalize(&text.value()) {
        Ok(v) => v,
        Err(e) => return syn::Error::new(text.span(), e).to_compile_error().into(),
    };
    let positional: Vec<&Arg> = args.iter().filter(|a| a.name.is_none()).collect();
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::new();
    for (which, spec) in &found {
        let (name, expr) = match which {
            Which::Index(n) => {
                let Some(a) = positional.get(*n) else {
                    return syn::Error::new(text.span(), format!("{{{n}}}: there is no argument {n}")).to_compile_error().into();
                };
                (n.to_string(), a.expr.clone())
            }
            Which::Name(n) => {
                let named = args.iter().find(|a| a.name.as_ref().is_some_and(|i| i == n));
                let expr = match named {
                    Some(a) => a.expr.clone(),
                    None => {
                        let id = Ident::new(n, Span::call_site());
                        syn::parse_quote!(#id)
                    }
                };
                (n.clone(), expr)
            }
        };
        if !seen.insert(name.clone()) {
            continue;
        }
        let fmt = LitStr::new(&format!("{{:{spec}}}"), text.span());
        entries.push(quote!((#name, ::std::format!(#fmt, #expr))));
    }
    let key = LitStr::new(&key, text.span());
    quote!(crate::i18n::tf(#key, &[#(#entries),*])).into()
}
