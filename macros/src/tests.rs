// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Unit tests for [`expand`](crate::expand).
//!
//! # Why the tests look like this
//!
//! A proc-macro crate can only export macros, so nothing here can
//! actually run the macro the way a user would. What it can do is call
//! `expand` with token streams and read what comes back. That is why
//! `expand` exists as its own function at all.
//!
//! The checks are on identifiers rather than on the printed text.
//! Printing a token stream puts spaces in places that depend on the
//! version of `proc-macro2`, so `contains("a :: b")` is a test of the
//! printer, not of the macro. [`idents`] flattens the tree instead, so
//! a check means "this name appears in the output" and nothing else.
//!
//! Whether the generated code *compiles* is a different question, and
//! `trybuild` is the tool for it. Those fixtures need the `doors`
//! crate, which cannot be a dependency here without a cycle, so they
//! live in `doors`.

use crate::expand;
use proc_macro2::{TokenStream, TokenTree};
use quote::quote;

/// Every identifier in a token stream, in order, groups included.
fn idents(tokens: &TokenStream) -> Vec<String> {
    let mut out = Vec::new();
    collect(tokens, &mut out);
    out
}

fn collect(tokens: &TokenStream, out: &mut Vec<String>) {
    for tree in tokens.clone() {
        match tree {
            TokenTree::Ident(ident) => out.push(ident.to_string()),
            TokenTree::Group(group) => collect(&group.stream(), out),
            _ => {}
        }
    }
}

/// Does the output mention this name?
fn has(tokens: &TokenStream, name: &str) -> bool {
    idents(tokens).iter().any(|found| found == name)
}

/// Did the macro reject its input?
fn rejected(tokens: &TokenStream) -> bool {
    has(tokens, "compile_error")
}

/// An `impl` with one method, marked however the caller likes.
fn greeter(attr: TokenStream) -> TokenStream {
    expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door #attr]
                fn hello(
                    &self,
                    req: Request<'_, Descriptors>,
                ) -> Result<Vec<u8>, MyError> {
                    todo!()
                }
            }
        },
    )
}

// ---------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------

/// No keyword at all means `procedure`, and the whole scaffold is
/// there: the trait, the constructor, the C entry point, the call
/// into the trampoline.
#[test]
fn the_default_shape_is_a_procedure() {
    let out = greeter(quote!(()));

    assert!(!rejected(&out));
    assert!(has(&out, "GreeterDoors"), "the extension trait");
    assert!(has(&out, "build_hello"), "the constructor");
    assert!(has(&out, "DoorBuilder"), "it extends the builder");
    assert!(has(&out, "run"), "it calls the trampoline");
    assert!(has(&out, "Outcome"), "it builds an Outcome");
    assert!(has(&out, "hello"), "it calls the user's method");
}

/// A bare `#[door]`, with no brackets at all, is the same thing.
#[test]
fn a_bare_door_attribute_is_accepted() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door]
                fn hello(
                    &self,
                    req: Request<'_, Descriptors>,
                ) -> Result<Vec<u8>, MyError> {
                    todo!()
                }
            }
        },
    );

    assert!(!rejected(&out));
    assert!(has(&out, "build_hello"));
}

#[test]
fn the_procedure_shape_can_be_named() {
    let out = greeter(quote!((procedure)));

    assert!(!rejected(&out));
    assert!(has(&out, "build_hello"));
    assert!(!has(&out, "decode_request"), "no serialisation here");
}

/// The `rpc` shape decodes and encodes through `doors`, and names the
/// feature it needs in the import it depends on.
#[test]
fn the_rpc_shape_calls_into_the_doors_codec() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door(rpc)]
                fn hello(&self, req: HelloReq)
                    -> Result<HelloResp, MyError>
                {
                    todo!()
                }
            }
        },
    );

    assert!(!rejected(&out));
    assert!(has(&out, "decode_request"));
    assert!(has(&out, "encode_reply"));
    assert!(
        has(&out, "__door_rpc_needs_the_doors_rpc_feature"),
        "the import that fails when the feature is off has to say so"
    );
}

#[test]
fn the_reply_buf_shape_lends_a_buffer() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door(reply_buf)]
                fn hello(
                    &self,
                    req: Request<'_, Descriptors>,
                    out: &mut ReplyBuf,
                ) -> Result<(), MyError> {
                    todo!()
                }
            }
        },
    );

    assert!(!rejected(&out));
    assert!(has(&out, "ReplyBuf"));
    assert!(has(&out, "overflow"), "an overflow must not be silent");
    assert!(has(&out, "build_hello"));
}

/// An `impl` with one `handback` method, marked however the caller
/// likes. The return type is the pair the shape asks for.
fn handback(attr: TokenStream) -> TokenStream {
    expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door #attr]
                fn hello(
                    &self,
                    req: Request<'_, Descriptors>,
                ) -> Result<(Vec<u8>, Vec<OwnedFd>), MyError> {
                    todo!()
                }
            }
        },
    )
}

/// The keyword is understood, and the shape it picks builds an
/// `Outcome` field by field. `Outcome::bytes` hard-codes an empty
/// descriptor list, so seeing it here would mean the descriptors were
/// dropped.
#[test]
fn the_handback_shape_returns_descriptors() {
    let out = handback(quote!((handback)));

    assert!(!rejected(&out));
    assert!(has(&out, "build_hello"), "the constructor");
    assert!(has(&out, "run"), "it calls the trampoline");
    assert!(has(&out, "Outcome"), "it builds an Outcome");
    assert!(has(&out, "descriptors"), "and fills the descriptors in");
    assert!(!has(&out, "bytes"), "`Outcome::bytes` would drop them");
}

/// It is a shape, so it cannot share a method with another one.
#[test]
fn handback_with_another_shape_is_rejected() {
    let out = handback(quote!((handback, procedure)));
    assert!(rejected(&out));
}

#[test]
fn handback_twice_is_rejected() {
    let out = handback(quote!((handback, handback)));
    assert!(rejected(&out));
}

/// Flags and parameters work on it like any other shape.
#[test]
fn handback_takes_the_ordinary_options() {
    let out = handback(quote!((handback, max_descriptors = 0)));

    assert!(!rejected(&out));
    assert!(has(&out, "max_descriptors"));
    assert!(has(&out, "check_builder_conflicts"));
}

/// `handback` and `procedure` take the same arguments, so counting
/// them cannot tell the two apart. A method that forgot the
/// descriptors is named here rather than inside generated code.
#[test]
fn a_handback_method_that_returns_only_bytes_is_rejected() {
    let out = greeter(quote!((handback)));
    assert!(rejected(&out));
}

/// A return type the macro cannot read is left to the compiler, which
/// knows what an alias means and the macro does not.
#[test]
fn an_unreadable_handback_return_type_is_left_alone() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door(handback)]
                fn hello(
                    &self,
                    req: Request<'_, Descriptors>,
                ) -> MyResult {
                    todo!()
                }
            }
        },
    );

    assert!(!rejected(&out));
}

/// `raw` is the C entry point already. No trampoline is written, and
/// the method itself is what the door is built with.
#[test]
fn the_raw_shape_generates_no_trampoline() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door(raw)]
                extern "C" fn hello(
                    cookie: *mut c_void,
                    argp: *mut c_char,
                    arg_size: usize,
                    dp: *mut door_desc_t,
                    n_desc: c_uint,
                ) {
                    todo!()
                }
            }
        },
    );

    assert!(!rejected(&out));
    assert!(has(&out, "build_hello"), "it still gets a constructor");
    assert!(!has(&out, "Outcome"), "no trampoline was written");
    assert!(!has(&out, "run"), "the user's function is the entry point");
}

#[test]
fn a_raw_method_must_be_extern_c() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door(raw)]
                fn hello(
                    cookie: *mut c_void,
                    argp: *mut c_char,
                    arg_size: usize,
                    dp: *mut door_desc_t,
                    n_desc: c_uint,
                ) {
                    todo!()
                }
            }
        },
    );

    assert!(rejected(&out));
}

// ---------------------------------------------------------------
// Flags
// ---------------------------------------------------------------

/// An `impl` whose own text names neither typestate, so a test can
/// tell what the macro chose from what the user wrote.
fn neutral(attr: TokenStream) -> TokenStream {
    expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door #attr]
                fn hello(&self, req: Req) -> Result<Vec<u8>, MyError> {
                    todo!()
                }
            }
        },
    )
}

/// `refuse_desc` picks the typestate that has no descriptor method,
/// and asks the builder for `DOOR_REFUSE_DESC`.
#[test]
fn refuse_desc_changes_the_request_typestate() {
    let out = neutral(quote!((refuse_desc)));

    assert!(!rejected(&out));
    assert!(has(&out, "NoDescriptors"));
    assert!(!has(&out, "Descriptors"), "the two are alternatives");
    assert!(has(&out, "refuse_descriptors"), "the builder is told too");
}

/// Without it, descriptors are allowed through.
#[test]
fn without_refuse_desc_descriptors_are_allowed() {
    let out = neutral(quote!(()));

    assert!(has(&out, "Descriptors"));
    assert!(!has(&out, "NoDescriptors"));
    assert!(!has(&out, "refuse_descriptors"));
}

#[test]
fn unref_dispatches_to_the_hook() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door(unref)]
                fn hello(
                    &self,
                    req: Request<'_, Descriptors>,
                ) -> Result<Vec<u8>, MyError> {
                    todo!()
                }

                fn on_unreferenced(&self) {}
            }
        },
    );

    assert!(!rejected(&out));
    assert!(has(&out, "is_unreferenced"), "the check is generated");
    assert!(has(&out, "on_unreferenced"), "and it calls the hook");
    assert!(has(&out, "unref"), "the builder is told too");
}

#[test]
fn unref_multi_dispatches_to_the_hook() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door(unref_multi)]
                fn hello(
                    &self,
                    req: Request<'_, Descriptors>,
                ) -> Result<Vec<u8>, MyError> {
                    todo!()
                }

                fn on_unreferenced(&self) {}
            }
        },
    );

    assert!(!rejected(&out));
    assert!(has(&out, "unref_multi"));
    assert!(has(&out, "is_unreferenced"));
}

/// A door that did not ask for the notification gets no check for it,
/// and needs no hook.
#[test]
fn without_unref_there_is_no_check_and_no_hook() {
    let out = greeter(quote!(()));

    assert!(!rejected(&out));
    assert!(!has(&out, "is_unreferenced"));
    assert!(!has(&out, "on_unreferenced"));
}

/// Asking for the notification without writing the hook is a mistake
/// the macro can see, so it says so instead of leaving the user with
/// an error inside generated code.
#[test]
fn unref_without_the_hook_is_rejected() {
    let out = greeter(quote!((unref)));
    assert!(rejected(&out));
}

#[test]
fn unref_multi_without_the_hook_is_rejected() {
    let out = greeter(quote!((unref_multi)));
    assert!(rejected(&out));
}

#[test]
fn private_asks_for_a_private_thread_pool() {
    let out = greeter(quote!((private)));

    assert!(!rejected(&out));
    assert!(has(&out, "private_pool"));
}

/// `untagged` is a real flag, and it is passed to the builder rather
/// than baked into the generated code, so that the attribute and
/// `DoorBuilder::untagged()` are one setting.
#[test]
fn untagged_reaches_the_builder() {
    let out = greeter(quote!((untagged)));

    assert!(!rejected(&out));
    assert!(has(&out, "untagged"), "the builder is told");
    assert!(has(&out, "reply_protocol"), "and the door is built from it");
}

/// Both entry points are always written, because which one is
/// registered is decided when the door is built, not here.
#[test]
fn without_untagged_the_builder_is_not_told() {
    let out = greeter(quote!(()));

    assert!(!rejected(&out));
    assert!(!has(&out, "untagged"), "nothing asked for it");
    assert!(has(&out, "ReplyProtocol"), "the choice is still made");
    assert!(has(&out, "Tagged"));
    assert!(has(&out, "Untagged"));
}

#[test]
fn a_repeated_untagged_is_rejected() {
    let out = greeter(quote!((untagged, untagged)));
    assert!(rejected(&out));
}

#[test]
fn request_size_reaches_the_builder() {
    let out = greeter(quote!((request_size = ..=8192)));

    assert!(!rejected(&out));
    assert!(has(&out, "request_size"));
    assert!(
        has(&out, "check_builder_conflicts"),
        "setting it twice has to be caught"
    );
}

/// A lower bound is kept as written; a missing one becomes zero.
#[test]
fn request_size_accepts_both_range_forms() {
    let out = greeter(quote!((request_size = 16..=8192)));

    assert!(!rejected(&out));
    assert!(has(&out, "request_size"));
}

#[test]
fn max_descriptors_reaches_the_builder() {
    let out = greeter(quote!((max_descriptors = 4)));

    assert!(!rejected(&out));
    assert!(has(&out, "max_descriptors"));
    assert!(has(&out, "check_builder_conflicts"));
}

/// A door that sets neither parameter cannot clash with the builder,
/// so no check is generated for it.
#[test]
fn no_parameters_means_no_conflict_check() {
    let out = greeter(quote!((refuse_desc)));

    assert!(!has(&out, "check_builder_conflicts"));
}

#[test]
fn every_option_can_be_used_at_once() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door(
                    rpc,
                    refuse_desc,
                    unref,
                    unref_multi,
                    private,
                    untagged,
                    request_size = 0..=8192,
                    max_descriptors = 2
                )]
                fn hello(&self, req: HelloReq)
                    -> Result<HelloResp, MyError>
                {
                    todo!()
                }

                fn on_unreferenced(&self) {}
            }
        },
    );

    assert!(!rejected(&out));
    assert!(has(&out, "refuse_descriptors"));
    assert!(has(&out, "unref_multi"));
    assert!(has(&out, "private_pool"));
    assert!(has(&out, "untagged"));
    assert!(has(&out, "request_size"));
    assert!(has(&out, "max_descriptors"));
}

// ---------------------------------------------------------------
// Rejections
// ---------------------------------------------------------------

#[test]
fn an_unknown_keyword_is_rejected() {
    let out = greeter(quote!((refuse_descriptors)));
    assert!(rejected(&out));
}

#[test]
fn two_shapes_are_rejected() {
    let out = greeter(quote!((rpc, procedure)));
    assert!(rejected(&out));
}

#[test]
fn the_same_shape_twice_is_rejected() {
    let out = greeter(quote!((rpc, rpc)));
    assert!(rejected(&out));
}

#[test]
fn a_repeated_flag_is_rejected() {
    let out = greeter(quote!((refuse_desc, refuse_desc)));
    assert!(rejected(&out));
}

#[test]
fn a_repeated_parameter_is_rejected() {
    let out = greeter(quote!((request_size = ..=8, request_size = ..=16)));
    assert!(rejected(&out));
}

#[test]
fn a_flag_with_a_value_is_rejected() {
    let out = greeter(quote!((refuse_desc = true)));
    assert!(rejected(&out));
}

#[test]
fn a_half_open_request_size_is_rejected() {
    let out = greeter(quote!((request_size = ..8192)));
    assert!(rejected(&out));
}

#[test]
fn an_open_ended_request_size_is_rejected() {
    let out = greeter(quote!((request_size = 16..)));
    assert!(rejected(&out));
}

#[test]
fn a_request_size_that_is_not_a_range_is_rejected() {
    let out = greeter(quote!((request_size = 8192)));
    assert!(rejected(&out));
}

#[test]
fn a_max_descriptors_that_is_not_a_number_is_rejected() {
    let out = greeter(quote!((max_descriptors = "four")));
    assert!(rejected(&out));
}

#[test]
fn options_on_the_outer_attribute_are_rejected() {
    let out = expand(
        quote!(rpc),
        quote! {
            impl Greeter {
                #[door]
                fn hello(
                    &self,
                    req: Request<'_, Descriptors>,
                ) -> Result<Vec<u8>, MyError> {
                    todo!()
                }
            }
        },
    );

    assert!(rejected(&out));
}

#[test]
fn something_that_is_not_an_impl_is_rejected() {
    let out = expand(
        TokenStream::new(),
        quote! {
            fn hello() {}
        },
    );

    assert!(rejected(&out));
}

#[test]
fn an_impl_with_no_door_methods_is_rejected() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                fn hello(&self) {}
            }
        },
    );

    assert!(rejected(&out));
}

#[test]
fn a_generic_impl_is_rejected() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl<T> Greeter<T> {
                #[door]
                fn hello(
                    &self,
                    req: Request<'_, Descriptors>,
                ) -> Result<Vec<u8>, MyError> {
                    todo!()
                }
            }
        },
    );

    assert!(rejected(&out));
}

#[test]
fn a_trait_impl_is_rejected() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greet for Greeter {
                #[door]
                fn hello(
                    &self,
                    req: Request<'_, Descriptors>,
                ) -> Result<Vec<u8>, MyError> {
                    todo!()
                }
            }
        },
    );

    assert!(rejected(&out));
}

#[test]
fn a_method_without_a_shared_receiver_is_rejected() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door]
                fn hello(
                    &mut self,
                    req: Request<'_, Descriptors>,
                ) -> Result<Vec<u8>, MyError> {
                    todo!()
                }
            }
        },
    );

    assert!(rejected(&out));
}

#[test]
fn a_method_with_the_wrong_number_of_arguments_is_rejected() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door]
                fn hello(&self) -> Result<Vec<u8>, MyError> {
                    todo!()
                }
            }
        },
    );

    assert!(rejected(&out));
}

#[test]
fn an_async_method_is_rejected() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door]
                async fn hello(
                    &self,
                    req: Request<'_, Descriptors>,
                ) -> Result<Vec<u8>, MyError> {
                    todo!()
                }
            }
        },
    );

    assert!(rejected(&out));
}

#[test]
fn a_generic_method_is_rejected() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door]
                fn hello<T>(
                    &self,
                    req: Request<'_, Descriptors>,
                ) -> Result<Vec<u8>, MyError> {
                    todo!()
                }
            }
        },
    );

    assert!(rejected(&out));
}

/// Several mistakes in one `impl` are reported together.
#[test]
fn every_bad_method_is_reported() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door(nonsense)]
                fn a(&self, req: Request<'_, Descriptors>) {}

                #[door(rubbish)]
                fn b(&self, req: Request<'_, Descriptors>) {}
            }
        },
    );

    let count = idents(&out)
        .iter()
        .filter(|found| *found == "compile_error")
        .count();
    assert_eq!(count, 2, "one message per mistake");
}

/// A rejection still prints the `impl` block, so the rest of the
/// user's crate does not collapse into "cannot find type `Greeter`".
#[test]
fn a_rejected_impl_is_still_emitted() {
    let out = greeter(quote!((nonsense)));

    assert!(rejected(&out));
    assert!(has(&out, "hello"), "the method survives");
    assert!(!has(&out, "door"), "but the inert marker does not");
}

// ---------------------------------------------------------------
// Hygiene (GOALS.md §3.7)
// ---------------------------------------------------------------

/// Generated code may name `doors::__private` and nothing else in
/// `doors`. Anything else would break the day the public API changes.
#[test]
fn generated_code_only_reaches_into_private() {
    let out = greeter(quote!((refuse_desc, request_size = ..=8192)));
    let names = idents(&out);

    for (index, name) in names.iter().enumerate() {
        if name == "doors" {
            assert_eq!(
                names.get(index + 1).map(String::as_str),
                Some("__private"),
                "every path into `doors` goes through `__private`"
            );
        }
    }
}

/// The wire format belongs to `doors`. This crate must not know that
/// it is `postcard`, or that `serde` is involved at all.
#[test]
fn generated_code_never_names_serde_or_postcard() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door(rpc)]
                fn hello(&self, req: HelloReq)
                    -> Result<HelloResp, MyError>
                {
                    todo!()
                }
            }
        },
    );

    assert!(!has(&out, "serde"));
    assert!(!has(&out, "postcard"));
}

/// Whatever comes out has to be parseable, error paths included. A
/// macro that emits broken tokens produces an error with no useful
/// span at all.
#[test]
fn the_output_always_parses() {
    let cases = vec![
        greeter(quote!(())),
        greeter(quote!((rpc))),
        greeter(quote!((nonsense))),
        greeter(quote!((unref))),
        greeter(quote!((handback))),
        handback(quote!((handback))),
        expand(
            TokenStream::new(),
            quote!(
                fn hello() {}
            ),
        ),
    ];

    for out in cases {
        let parsed: syn::Result<syn::File> = syn::parse2(out.clone());
        assert!(parsed.is_ok(), "did not parse: {out}");
    }
}

// ---------------------------------------------------------------
// Shape of the generated API
// ---------------------------------------------------------------

/// One trait, one method per door, however many doors there are.
#[test]
fn several_doors_share_one_trait() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door]
                fn hello(
                    &self,
                    req: Request<'_, Descriptors>,
                ) -> Result<Vec<u8>, MyError> {
                    todo!()
                }

                #[door(refuse_desc)]
                fn ping(
                    &self,
                    req: Request<'_, NoDescriptors>,
                ) -> Result<Vec<u8>, MyError> {
                    todo!()
                }
            }
        },
    );

    assert!(!rejected(&out));
    assert!(has(&out, "build_hello"));
    assert!(has(&out, "build_ping"));

    let traits = idents(&out)
        .iter()
        .filter(|found| *found == "GreeterDoors")
        .count();
    assert_eq!(traits, 2, "declared once, implemented once");
}

/// A public method needs a public trait, or nobody outside the module
/// could finish the builder chain.
#[test]
fn a_public_door_gets_a_public_trait() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[door]
                pub fn hello(
                    &self,
                    req: Request<'_, Descriptors>,
                ) -> Result<Vec<u8>, MyError> {
                    todo!()
                }
            }
        },
    );

    let text = out.to_string();
    assert!(
        text.contains("pub trait GreeterDoors"),
        "expected a public trait in: {text}"
    );
}

/// A method that may not exist gets a constructor that may not exist
/// either, or the trait would promise something that is not there.
#[test]
fn a_cfg_on_the_method_reaches_the_constructor() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Greeter {
                #[cfg(feature = "extras")]
                #[door]
                fn hello(&self, req: Req) -> Result<Vec<u8>, MyError> {
                    todo!()
                }
            }
        },
    );

    assert!(!rejected(&out));

    let count = idents(&out).iter().filter(|found| *found == "cfg").count();
    assert_eq!(
        count, 3,
        "the method keeps its own, and both halves of the trait get one"
    );
}

/// The trait is named after the type, so two servers in one module do
/// not collide.
#[test]
fn the_trait_is_named_after_the_type() {
    let out = expand(
        TokenStream::new(),
        quote! {
            impl Counter {
                #[door]
                fn bump(
                    &self,
                    req: Request<'_, Descriptors>,
                ) -> Result<Vec<u8>, MyError> {
                    todo!()
                }
            }
        },
    );

    assert!(has(&out, "CounterDoors"));
    assert!(!has(&out, "GreeterDoors"));
}
