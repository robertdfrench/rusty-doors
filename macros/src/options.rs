// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Reading the per-method `#[door(...)]` attribute.
//!
//! `#[door]` is not a real proc macro. It is an inert marker that
//! `#[doors::server]` reads and then removes, which is why everything
//! about it is parsed here by hand rather than by the compiler.
//!
//! The grammar is small on purpose:
//!
//! ```text
//! #[door(shape?, flag*, request_size = <range>?, max_descriptors = <n>?)]
//! ```
//!
//! Every mistake is a [`syn::Error`] with a span, never a panic
//! (`GOALS.md` §3.7). A macro that panics reports the wrong file and
//! no line at all, so the user is left guessing.

use proc_macro2::Span;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Attribute, Error, Expr, Ident, LitInt, Meta, Result, Token};

/// Every keyword `#[door(...)]` understands, for error messages.
const KEYWORDS: &str = "procedure, rpc, reply_buf, handback, raw, \
     refuse_desc, unref, unref_multi, private, untagged, \
     request_size, max_descriptors";

/// What kind of function the user wrote.
///
/// A method has exactly one shape. The shape decides what the
/// generated trampoline does between the kernel and the user's code,
/// so two shapes at once would be two different pieces of generated
/// code for one function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    /// `fn(&self, Request<'_, D>) -> Result<Vec<u8>, E>`
    Procedure,
    /// `fn(&self, Req) -> Result<Resp, E>`
    Rpc,
    /// `fn(&self, Request<'_, D>, &mut ReplyBuf) -> Result<(), E>`
    ReplyBuf,
    /// `fn(&self, Request<'_, D>) -> Result<(Vec<u8>, Vec<OwnedFd>), E>`
    ///
    /// The same as [`Shape::Procedure`], except that the reply may
    /// also carry descriptors. It is the only shape that can hand one
    /// back.
    ///
    /// # A reply carries at most sixteen descriptors
    ///
    /// `doors` copies them into a fixed array of
    /// `MAX_REPLY_DESCRIPTORS`, which is sixteen. Any descriptor past
    /// that is closed, not sent, and the call still succeeds. So a
    /// method that returns more than sixteen loses the extra ones with
    /// no error anywhere. Return sixteen or fewer.
    ///
    /// # Do not add `refuse_desc`
    ///
    /// That flag stops descriptors in both directions, so no client
    /// could read what this shape sends back. Use
    /// `max_descriptors = 0` to turn away descriptors the *caller*
    /// sends.
    Handback,
    /// The C server procedure, passed through untouched.
    Raw,
}

impl Shape {
    /// The keyword that selects this shape.
    pub fn keyword(self) -> &'static str {
        match self {
            Shape::Procedure => "procedure",
            Shape::Rpc => "rpc",
            Shape::ReplyBuf => "reply_buf",
            Shape::Handback => "handback",
            Shape::Raw => "raw",
        }
    }

    fn from_ident(ident: &Ident) -> Option<Shape> {
        match ident.to_string().as_str() {
            "procedure" => Some(Shape::Procedure),
            "rpc" => Some(Shape::Rpc),
            "reply_buf" => Some(Shape::ReplyBuf),
            "handback" => Some(Shape::Handback),
            "raw" => Some(Shape::Raw),
            _ => None,
        }
    }

    /// How many arguments the method must take, receiver included.
    ///
    /// `raw` has no receiver: it is the C entry point itself, and the
    /// kernel knows nothing about `self`.
    pub fn arity(self) -> usize {
        match self {
            Shape::Procedure | Shape::Rpc | Shape::Handback => 2,
            Shape::ReplyBuf => 3,
            Shape::Raw => 5,
        }
    }

    /// A sentence describing the signature, for error messages.
    pub fn signature(self) -> &'static str {
        match self {
            Shape::Procedure => {
                "fn(&self, Request<'_, D>) -> Result<Vec<u8>, E>"
            }
            Shape::Rpc => "fn(&self, Req) -> Result<Resp, E>",
            Shape::ReplyBuf => {
                "fn(&self, Request<'_, D>, &mut ReplyBuf) -> Result<(), E>"
            }
            Shape::Handback => {
                "fn(&self, Request<'_, D>) \
                 -> Result<(Vec<u8>, Vec<OwnedFd>), E>"
            }
            Shape::Raw => {
                "extern \"C\" fn(*mut c_void, *mut c_char, usize, \
                 *mut door_desc_t, c_uint)"
            }
        }
    }
}

/// The smallest and largest request the door will accept.
///
/// Stored as two expressions rather than one range, because the
/// builder wants a `RangeInclusive` and the user is allowed to write
/// `..=8192` with no lower bound. The missing bound becomes `0`.
pub struct RequestSize {
    /// The lower bound. `None` when the user wrote `..=n`.
    pub start: Option<Expr>,
    /// The upper bound. Always present; an open end is rejected.
    pub end: Expr,
}

/// Everything one `#[door(...)]` said.
pub struct DoorOptions {
    /// The shape. `procedure` when the user named none.
    pub shape: Shape,
    /// Where the shape keyword was, or the attribute if it defaulted.
    pub shape_span: Span,
    /// `DOOR_REFUSE_DESC`. Also picks the `Request` typestate.
    pub refuse_desc: bool,
    /// `DOOR_UNREF`.
    pub unref: bool,
    /// `DOOR_UNREF_MULTI`.
    pub unref_multi: bool,
    /// `DOOR_PRIVATE`.
    pub private: bool,
    /// Reply with no §3.9 status byte.
    ///
    /// Not a kernel attribute like the flags above. It changes what
    /// the generated code writes back, so that a caller which does
    /// not use this crate can read the reply (`GOALS.md` §6.5).
    pub untagged: bool,
    /// `DOOR_PARAM_DATA_MIN` and `DOOR_PARAM_DATA_MAX`.
    pub request_size: Option<RequestSize>,
    /// `DOOR_PARAM_DESC_MAX`.
    pub max_descriptors: Option<LitInt>,
    /// The whole attribute, for errors that belong to no one keyword.
    pub span: Span,
}

impl DoorOptions {
    fn new(span: Span) -> Self {
        DoorOptions {
            shape: Shape::Procedure,
            shape_span: span,
            refuse_desc: false,
            unref: false,
            unref_multi: false,
            private: false,
            untagged: false,
            request_size: None,
            max_descriptors: None,
            span,
        }
    }

    /// Does this door ask for unreferenced notifications?
    ///
    /// Either flag means the user must also write `on_unreferenced`,
    /// and the generated code must check for the notification before
    /// it treats the call as a real request.
    pub fn wants_unref(&self) -> bool {
        self.unref || self.unref_multi
    }
}

/// One `keyword` or `keyword = value` inside the attribute.
struct Opt {
    name: Ident,
    value: Option<Expr>,
}

impl Parse for Opt {
    fn parse(input: ParseStream) -> Result<Self> {
        let name: Ident = input.parse()?;
        let value = if input.peek(Token![=]) {
            let _: Token![=] = input.parse()?;
            Some(input.parse::<Expr>()?)
        } else {
            None
        };
        Ok(Opt { name, value })
    }
}

/// Read one `#[door(...)]` attribute.
pub fn parse(attr: &Attribute) -> Result<DoorOptions> {
    let span = attr.span();
    let mut opts = DoorOptions::new(span);

    let list = match &attr.meta {
        // A bare `#[door]`. Everything defaults.
        Meta::Path(_) => return Ok(opts),
        Meta::List(list) => list,
        Meta::NameValue(nv) => {
            return Err(Error::new(
                nv.span(),
                "`#[door]` takes options in brackets, like \
                 `#[door(rpc, refuse_desc)]`",
            ))
        }
    };

    let items =
        list.parse_args_with(Punctuated::<Opt, Token![,]>::parse_terminated)?;

    // Remembered so a second shape keyword can name the first one.
    let mut shape: Option<(Shape, Span)> = None;

    for opt in items {
        let name = opt.name.to_string();
        let at = opt.name.span();

        if let Some(found) = Shape::from_ident(&opt.name) {
            no_value(&opt, "a shape")?;
            match shape {
                Some((first, _)) if first == found => {
                    return Err(Error::new(
                        at,
                        format!("`{name}` is set twice"),
                    ))
                }
                Some((first, _)) => {
                    return Err(Error::new(
                        at,
                        format!(
                            "`{}` and `{}` are both shapes; a method \
                             has exactly one",
                            first.keyword(),
                            found.keyword()
                        ),
                    ))
                }
                None => shape = Some((found, at)),
            }
            continue;
        }

        match name.as_str() {
            "refuse_desc" => {
                no_value(&opt, "a flag")?;
                flag(&mut opts.refuse_desc, &name, at)?;
            }
            "unref" => {
                no_value(&opt, "a flag")?;
                flag(&mut opts.unref, &name, at)?;
            }
            "unref_multi" => {
                no_value(&opt, "a flag")?;
                flag(&mut opts.unref_multi, &name, at)?;
            }
            "private" => {
                no_value(&opt, "a flag")?;
                flag(&mut opts.private, &name, at)?;
            }
            "untagged" => {
                no_value(&opt, "a flag")?;
                flag(&mut opts.untagged, &name, at)?;
            }
            "request_size" => {
                if opts.request_size.is_some() {
                    return Err(Error::new(at, "`request_size` is set twice"));
                }
                opts.request_size = Some(request_size(&opt, at)?);
            }
            "max_descriptors" => {
                if opts.max_descriptors.is_some() {
                    return Err(Error::new(
                        at,
                        "`max_descriptors` is set twice",
                    ));
                }
                opts.max_descriptors = Some(max_descriptors(&opt, at)?);
            }
            _ => {
                return Err(Error::new(
                    at,
                    format!(
                        "unknown `#[door(...)]` option `{name}`; \
                         expected one of: {KEYWORDS}"
                    ),
                ))
            }
        }
    }

    if let Some((found, at)) = shape {
        opts.shape = found;
        opts.shape_span = at;
    }

    Ok(opts)
}

/// Refuse `keyword = value` where only `keyword` makes sense.
fn no_value(opt: &Opt, kind: &str) -> Result<()> {
    match &opt.value {
        None => Ok(()),
        Some(v) => Err(Error::new(
            v.span(),
            format!("`{}` is {kind} and takes no value", opt.name),
        )),
    }
}

/// Set a flag, refusing a second mention of it.
fn flag(slot: &mut bool, name: &str, at: Span) -> Result<()> {
    if *slot {
        return Err(Error::new(at, format!("`{name}` is set twice")));
    }
    *slot = true;
    Ok(())
}

/// `request_size = ..=8192` or `request_size = 0..=8192`.
fn request_size(opt: &Opt, at: Span) -> Result<RequestSize> {
    let value = opt.value.as_ref().ok_or_else(|| {
        Error::new(
            at,
            "`request_size` needs a range, like `request_size = ..=8192`",
        )
    })?;

    let range =
        match value {
            Expr::Range(range) => range,
            other => return Err(Error::new(
                other.span(),
                "`request_size` takes a range, like `..=8192` or `0..=8192`",
            )),
        };

    // Half-open is refused rather than quietly turned into `..=n-1`.
    // `DOOR_PARAM_DATA_MAX` is a maximum the kernel enforces, and
    // being one byte out is exactly the sort of bug nobody finds.
    if !matches!(range.limits, syn::RangeLimits::Closed(_)) {
        return Err(Error::new(
            range.span(),
            "`request_size` needs an inclusive range: write `..=8192`, \
             not `..8192`",
        ));
    }

    let end = match &range.end {
        Some(end) => (**end).clone(),
        None => {
            return Err(Error::new(
                range.span(),
                "`request_size` needs an upper bound, like `..=8192`",
            ))
        }
    };

    Ok(RequestSize {
        start: range.start.as_ref().map(|s| (**s).clone()),
        end,
    })
}

/// `max_descriptors = 4`.
fn max_descriptors(opt: &Opt, at: Span) -> Result<LitInt> {
    let value = opt.value.as_ref().ok_or_else(|| {
        Error::new(
            at,
            "`max_descriptors` needs a number, like \
             `max_descriptors = 4`",
        )
    })?;

    match value {
        Expr::Lit(lit) => match &lit.lit {
            syn::Lit::Int(int) => Ok(int.clone()),
            other => Err(Error::new(
                other.span(),
                "`max_descriptors` takes a whole number, like 4",
            )),
        },
        other => Err(Error::new(
            other.span(),
            "`max_descriptors` takes a whole number, like 4",
        )),
    }
}
