// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The macro behind `#[doors::server]`.
//!
//! You should not depend on this crate directly. `doors` re-exports
//! the one macro it holds, and the code it writes calls into
//! `doors::__private`, so the two crates only work as a pair.
//!
//! # What the macro does
//!
//! An illumos door is a C function pointer plus a cookie. The kernel
//! calls that function on a thread of its own choosing and expects it
//! to end with `door_return(3C)`, which never comes back. Writing that
//! by hand is easy to get wrong, so this macro writes it for you from
//! an ordinary Rust method.
//!
//! ```ignore
//! struct Greeter { greeting: String }
//!
//! #[doors::server]
//! impl Greeter {
//!     #[door(refuse_desc)]
//!     fn hello(&self, req: Request<'_, NoDescriptors>)
//!         -> Result<Vec<u8>, std::io::Error>
//!     {
//!         Ok(self.greeting.clone().into_bytes())
//!     }
//! }
//!
//! let door = Door::builder(Greeter { greeting: "hi".into() })
//!     .thread_stack_size(256 * 1024)
//!     .build_hello()?;
//! ```
//!
//! The methods live on an `impl` block so they can take `&self`. That
//! `&self` is the door's cookie, resolved on every call.
//!
//! # One macro only
//!
//! There is one `#[proc_macro_attribute]` here, `server`. Everything
//! else is said with `#[door(...)]` on a method, which is an inert
//! marker: the compiler never sees it, because `#[doors::server]`
//! reads it and takes it away.
//!
//! One macro rather than one per shape means the options can be
//! checked against each other. `#[door(rpc, procedure)]` is a mistake
//! we can name, where two separate macros would each be happy.
//!
//! # Options
//!
//! A shape, at most one. It says what the method's signature is. When
//! none is given the shape is `procedure`.
//!
//! | Keyword | Signature |
//! |---|---|
//! | `procedure` | `fn(&self, Request<'_, D>) -> Result<Vec<u8>, E>` |
//! | `rpc` | `fn(&self, Req) -> Result<Resp, E>` |
//! | `reply_buf` | `fn(&self, Request<'_, D>, &mut ReplyBuf) -> Result<(), E>` |
//! | `handback` | `fn(&self, Request<'_, D>) -> Result<(Vec<u8>, Vec<OwnedFd>), E>` |
//! | `raw` | the C server procedure, passed through untouched |
//!
//! Flags, in any combination:
//!
//! - `refuse_desc` — the door refuses descriptors. `D` becomes
//!   `NoDescriptors`, which has no method that reaches one.
//! - `unref` — ask for an unreferenced notification.
//! - `unref_multi` — ask for repeated ones.
//! - `private` — give this door its own pool of server threads.
//! - `request_size = ..=8192` — the largest request accepted.
//! - `max_descriptors = 4` — the most descriptors one call may carry.
//!
//! `DOOR_NO_CANCEL` is not an option. The builder always sets it, and
//! there is no way to clear it.
//!
//! ## Sending descriptors back with `handback`
//!
//! `handback` is the one shape whose reply can carry descriptors. It
//! is otherwise `procedure`: same arguments, and the bytes come back
//! the same way.
//!
//! ```ignore
//! #[door(handback, max_descriptors = 0)]
//! fn stream(&self, req: Request<'_, Descriptors>)
//!     -> Result<(Vec<u8>, Vec<OwnedFd>), std::io::Error>
//! {
//!     Ok((b"here".to_vec(), vec![self.open_log()?]))
//! }
//! ```
//!
//! Three things to know before you use it.
//!
//! **A reply carries at most sixteen descriptors.** `doors` copies
//! them into a fixed array of `MAX_REPLY_DESCRIPTORS`, which is
//! sixteen. Any descriptor past that is closed, not sent, and the call
//! still succeeds. So returning more than sixteen loses the extra ones
//! with no error anywhere. Return sixteen or fewer.
//!
//! **Do not add `refuse_desc`.** That flag stops descriptors in both
//! directions, so no client of this door could read what it sends
//! back. Use `max_descriptors = 0` when you want to turn away
//! descriptors the *caller* sends: it leaves the reply direction
//! working.
//!
//! **The caller has to ask for them too.** A client only reads
//! descriptors out of a reply if it was built with
//! `Client::with_descriptors()`. A plain client closes them and fails
//! the call.
//!
//! An `Err` return sends no descriptors. There are none to send: the
//! method returns a pair only when it succeeds.
//!
//! # Testing
//!
//! All the work happens in `expand`, which takes and returns
//! `proc_macro2::TokenStream`. A proc-macro crate can only export
//! macros, but it can still test its own functions, and the unit tests
//! at the bottom of this crate call `expand` directly.

extern crate proc_macro;

mod codegen;
mod options;

#[cfg(test)]
mod tests;

use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Error, ItemImpl};

/// Everything `#[doors::server]` does.
///
/// Separate from the exported macro so it can be tested: this
/// function takes and returns `proc_macro2` token streams, which
/// exist outside a compiler, while `proc_macro` ones do not.
///
/// It never panics and never returns `Err`. A bad input comes back as
/// `compile_error!` tokens, followed where possible by the user's own
/// `impl` block with the `#[door(...)]` markers removed. Emitting the
/// block again matters: without it every use of the type would fail
/// too, and the one real error would be lost in the noise.
fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut out = TokenStream::new();

    // The outer attribute takes no options; they all go on methods.
    // Saying so is friendlier than ignoring what the user wrote.
    if !attr.is_empty() {
        out.extend(
            Error::new(
                attr.span(),
                "`#[doors::server]` takes no options; they go on each \
                 method, in `#[door(...)]`",
            )
            .to_compile_error(),
        );
    }

    let mut input: ItemImpl = match syn::parse2(item) {
        Ok(input) => input,
        Err(err) => {
            out.extend(err.to_compile_error());
            return out;
        }
    };

    match codegen::expand_impl(&mut input) {
        Ok(generated) => {
            out.extend(quote!(#input));
            out.extend(generated);
        }
        Err(err) => {
            out.extend(err.to_compile_error());
            out.extend(quote!(#input));
        }
    }

    out
}

/// Turn an `impl` block into a door server.
///
/// See the [crate docs](crate) for the options. Users reach this as
/// `#[doors::server]`.
#[proc_macro_attribute]
pub fn server(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    expand(attr.into(), item.into()).into()
}
