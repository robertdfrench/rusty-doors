// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Turning a checked `impl` block into code.
//!
//! # What comes out
//!
//! For an `impl Greeter` with a `#[door]` method `hello`, three things
//! are emitted:
//!
//! 1. The `impl` block itself, with every `#[door(...)]` removed.
//! 2. A trait `GreeterDoors` with one method `build_hello`.
//! 3. An implementation of that trait for `DoorBuilder<Greeter>`.
//!
//! # Why a trait and not an inherent `impl`
//!
//! `build_hello` has to hang off `DoorBuilder`, which lives in another
//! crate. Rust forbids an inherent `impl` on a foreign type (E0116),
//! and it forbids a foreign trait on a foreign type as well (E0117).
//! A trait defined right here in the user's crate is the one thing
//! left, and it reads the same at the call site:
//!
//! ```ignore
//! let door = Door::builder(state).build_hello()?;
//! ```
//!
//! # Where the generated code may look
//!
//! Only at `doors::__private` (`GOALS.md` §3.7). Nothing else in
//! `doors` is named, `serde` and `postcard` are never named, and every
//! call is written in full path form so that a trait the user happens
//! to have imported cannot change what runs.

use crate::options::{DoorOptions, Shape};
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, ToTokens};
use syn::spanned::Spanned;
use syn::{
    Attribute, Error, FnArg, GenericArgument, Ident, ImplItem, ImplItemFn,
    ItemImpl, PathArguments, Result, ReturnType, Signature, Type, Visibility,
};

/// The one module generated code is allowed to name.
fn private() -> TokenStream {
    quote!(::doors::__private)
}

/// Several problems reported at once.
///
/// A user who wrote three bad attributes should see three messages,
/// not one message three builds in a row.
#[derive(Default)]
struct Errors(Option<Error>);

impl Errors {
    fn push(&mut self, err: Error) {
        match &mut self.0 {
            Some(first) => first.combine(err),
            slot => *slot = Some(err),
        }
    }

    fn into_result(self) -> Result<()> {
        match self.0 {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

/// A method that carried a `#[door(...)]`.
struct DoorFn<'a> {
    method: &'a ImplItemFn,
    opts: DoorOptions,
}

/// Take every `#[door(...)]` off the methods of this `impl`.
///
/// Done first and separately, because the attribute is inert: if it
/// were left in place the compiler would report "cannot find attribute
/// `door`" and bury whatever error we are actually trying to show.
/// Returns the attributes it removed, one list per method position.
fn strip(input: &mut ItemImpl) -> Vec<(usize, Vec<Attribute>)> {
    let mut taken = Vec::new();

    for (index, item) in input.items.iter_mut().enumerate() {
        let ImplItem::Fn(method) = item else {
            continue;
        };

        let mut mine = Vec::new();
        method.attrs.retain(|attr| {
            if attr.path().is_ident("door") {
                mine.push(attr.clone());
                false
            } else {
                true
            }
        });

        if !mine.is_empty() {
            taken.push((index, mine));
        }
    }

    taken
}

/// Read the `impl` block and produce the trait and its implementation.
///
/// `input` is stripped of `#[door(...)]` before anything can fail, so
/// the caller can print the `impl` alongside an error and give the
/// user one clear message instead of a cascade.
pub fn expand_impl(input: &mut ItemImpl) -> Result<TokenStream> {
    let taken = strip(input);
    let mut errors = Errors::default();

    if let Some((_, path, _)) = &input.trait_ {
        errors.push(Error::new(
            path.span(),
            "`#[doors::server]` goes on an inherent `impl`, not on a \
             trait implementation",
        ));
    }

    if !input.generics.params.is_empty() {
        errors.push(Error::new(
            input.generics.span(),
            "`#[doors::server]` does not support a generic `impl`; the \
             door's state type has to be one concrete type",
        ));
    }

    let self_ty = &*input.self_ty;
    let ty_name = match self_ty_ident(self_ty) {
        Ok(name) => Some(name),
        Err(err) => {
            errors.push(err);
            None
        }
    };

    if taken.is_empty() {
        errors.push(Error::new(
            Span::call_site(),
            "no method in this `impl` is marked `#[door(...)]`, so \
             `#[doors::server]` has nothing to build",
        ));
    }

    // Collect the marked methods, reporting every bad attribute
    // rather than stopping at the first.
    let mut doors: Vec<DoorFn<'_>> = Vec::new();
    for (index, attrs) in &taken {
        let ImplItem::Fn(method) = &input.items[*index] else {
            continue;
        };

        if attrs.len() > 1 {
            errors.push(Error::new(
                attrs[1].span(),
                "a method takes at most one `#[door(...)]`",
            ));
        }

        match crate::options::parse(&attrs[0]) {
            Ok(opts) => match check_signature(method, &opts) {
                Ok(()) => doors.push(DoorFn { method, opts }),
                Err(err) => errors.push(err),
            },
            Err(err) => errors.push(err),
        }
    }

    // `on_unreferenced` is required exactly when some door asked for
    // an unreferenced notification, and forbidden nowhere. A door that
    // did not ask simply never calls it.
    let has_unref_hook = input.items.iter().any(|item| match item {
        ImplItem::Fn(f) => f.sig.ident == "on_unreferenced",
        _ => false,
    });

    for door in &doors {
        if door.opts.wants_unref() && !has_unref_hook {
            errors.push(Error::new(
                door.opts.span,
                "this door asks for an unreferenced notification, so \
                 the `impl` must also have `fn on_unreferenced(&self)`",
            ));
        }
    }

    errors.into_result()?;

    let ty_name = match ty_name {
        Some(name) => name,
        // Unreachable: a missing name is already an error above.
        None => return Ok(TokenStream::new()),
    };

    Ok(build_trait(self_ty, &ty_name, &doors))
}

/// The name of the type this `impl` is for.
///
/// It has to be one plain name: it becomes half of the generated
/// trait's name, and a generic or a reference has no name to use.
fn self_ty_ident(self_ty: &Type) -> Result<Ident> {
    if let Type::Path(path) = self_ty {
        if path.qself.is_none() {
            if let Some(last) = path.path.segments.last() {
                if last.arguments.is_none() {
                    return Ok(last.ident.clone());
                }
            }
        }
    }

    Err(Error::new(
        self_ty.span(),
        "`#[doors::server]` needs an `impl` on a plain named type, \
         like `impl Greeter`",
    ))
}

/// Check the parts of a signature the generated code depends on.
///
/// Only the parts that would otherwise produce a confusing error
/// later. Types are left to the compiler: it knows them better, and
/// its message points at the user's own line.
fn check_signature(method: &ImplItemFn, opts: &DoorOptions) -> Result<()> {
    let sig = &method.sig;
    let shape = opts.shape;

    if sig.asyncness.is_some() {
        return Err(Error::new(
            sig.span(),
            "a door method cannot be `async`; the kernel calls it on \
             a server thread and waits for it to finish",
        ));
    }

    if !sig.generics.params.is_empty() {
        return Err(Error::new(
            sig.generics.span(),
            "a door method cannot be generic; the kernel is given one \
             function pointer",
        ));
    }

    let receiver = sig.inputs.first().and_then(|arg| match arg {
        FnArg::Receiver(r) => Some(r),
        FnArg::Typed(_) => None,
    });

    if shape == Shape::Raw {
        if receiver.is_some() {
            return Err(Error::new(
                sig.span(),
                "a `#[door(raw)]` method takes no `self`; it is the C \
                 entry point, and the kernel knows nothing about self",
            ));
        }
        match &sig.abi {
            Some(abi) => {
                let c = match &abi.name {
                    Some(name) => name.value() == "C",
                    None => true,
                };
                if !c {
                    return Err(Error::new(
                        abi.span(),
                        "a `#[door(raw)]` method must be `extern \"C\"`",
                    ));
                }
            }
            None => {
                return Err(Error::new(
                    sig.span(),
                    "a `#[door(raw)]` method must be `extern \"C\"`",
                ))
            }
        }
    } else {
        match receiver {
            Some(r) if r.reference.is_some() && r.mutability.is_none() => {}
            _ => {
                return Err(Error::new(
                    sig.span(),
                    "a door method takes `&self`; the door's state is \
                     shared by every server thread, so it cannot be \
                     borrowed mutably",
                ))
            }
        }
    }

    if sig.inputs.len() != shape.arity() {
        return Err(Error::new(
            sig.inputs.span(),
            format!(
                "a `#[door({})]` method has the signature `{}`",
                shape.keyword(),
                shape.signature()
            ),
        ));
    }

    if shape == Shape::Handback {
        check_handback_return(sig)?;
    }

    Ok(())
}

/// Catch the one `handback` mistake worth naming here.
///
/// `handback` takes the same arguments as `procedure` and differs only
/// in what it returns, so the arity check above cannot tell them
/// apart. The easy slip is to write `Result<Vec<u8>, E>` and forget
/// the descriptors. Generated code then takes that apart as a pair,
/// and the compiler complains about a line the user never wrote.
///
/// Only a plainly wrong `Result<..>` is refused. A return type this
/// cannot read — an alias of the user's own, say — is left to the
/// compiler, which knows more about types than the macro ever will.
fn check_handback_return(sig: &Signature) -> Result<()> {
    let ReturnType::Type(_, ty) = &sig.output else {
        return Ok(());
    };
    let Type::Path(path) = &**ty else {
        return Ok(());
    };
    let Some(last) = path.path.segments.last() else {
        return Ok(());
    };
    if last.ident != "Result" {
        return Ok(());
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return Ok(());
    };
    let Some(GenericArgument::Type(ok)) = args.args.first() else {
        return Ok(());
    };
    if matches!(ok, Type::Tuple(_)) {
        return Ok(());
    }

    Err(Error::new(
        ok.span(),
        "a `#[door(handback)]` method returns the reply bytes and the \
         descriptors together, as a pair: `Result<(Vec<u8>, \
         Vec<OwnedFd>), E>`",
    ))
}

/// Emit the extension trait and its implementation.
fn build_trait(
    self_ty: &Type,
    ty_name: &Ident,
    doors: &[DoorFn<'_>],
) -> TokenStream {
    let p = private();
    let trait_name = format_ident!("{}Doors", ty_name);
    let vis = widest_visibility(doors);

    let mut declarations = Vec::new();
    let mut definitions = Vec::new();

    for door in doors {
        let name = &door.method.sig.ident;
        let build = format_ident!("build_{}", name);
        let doc = format!(
            "Create the door served by `{ty_name}::{name}`.\n\n\
             This replaces `build()`. It knows which server procedure \
             to register, so it takes no argument."
        );
        let body = build_body(self_ty, door);

        // A method behind a `#[cfg]` needs its constructor behind the
        // same one, or the trait would promise something that is not
        // there.
        let cfgs = cfg_attrs(door.method);

        declarations.push(quote! {
            #(#cfgs)*
            #[doc = #doc]
            fn #build(
                self,
            ) -> ::core::result::Result<#p::Door<#self_ty>, #p::Error>;
        });

        definitions.push(quote! {
            #(#cfgs)*
            fn #build(
                self,
            ) -> ::core::result::Result<#p::Door<#self_ty>, #p::Error> {
                #body
            }
        });
    }

    let doc = format!(
        "Doors served by `{ty_name}`.\n\n\
         Written by `#[doors::server]`. Each method finishes a \
         `Door::builder({ty_name} {{ .. }})` chain."
    );

    quote! {
        #[doc = #doc]
        #vis trait #trait_name {
            #(#declarations)*
        }

        impl #trait_name for #p::DoorBuilder<#self_ty> {
            #(#definitions)*
        }
    }
}

/// Every `#[cfg(...)]` on a method, to be copied onto its
/// constructor.
///
/// `#[cfg_attr]` is left alone: it decides what other attributes a
/// method has, not whether the method exists.
fn cfg_attrs(method: &ImplItemFn) -> Vec<&Attribute> {
    method
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .collect()
}

/// How visible the generated trait should be.
///
/// An `impl` block has no visibility of its own, so the trait copies
/// the widest visibility among the door methods. Anywhere a user can
/// call `Greeter::hello`, they can also name `GreeterDoors`.
fn widest_visibility(doors: &[DoorFn<'_>]) -> Visibility {
    let rank = |vis: &Visibility| match vis {
        Visibility::Public(_) => 2,
        Visibility::Restricted(_) => 1,
        Visibility::Inherited => 0,
    };

    let mut widest = Visibility::Inherited;
    for door in doors {
        if rank(&door.method.vis) > rank(&widest) {
            widest = door.method.vis.clone();
        }
    }
    widest
}

/// The body of one `build_foo`.
fn build_body(self_ty: &Type, door: &DoorFn<'_>) -> TokenStream {
    let p = private();
    let opts = &door.opts;
    let name = &door.method.sig.ident;

    // `raw` is the C entry point already, so there is nothing to wrap
    // and the method itself is what the kernel gets. It writes its own
    // reply, so the reply protocol has nothing to say about it.
    //
    // Every other shape gets two entry points, one per protocol, and
    // the door is created with whichever the builder asks for. The
    // kernel calls a server procedure with five arguments of its own
    // and leaves no room for a sixth, so the protocol cannot be passed
    // in at call time; the choice has to be which function is
    // registered.
    let (definition, create) = if opts.shape == Shape::Raw {
        (
            TokenStream::new(),
            quote!(#p::DoorBuilder::build(__builder, #self_ty::#name)),
        )
    } else {
        let dispatch = format_ident!("__door_dispatch_{}", name);
        let tagged = format_ident!("__door_server_procedure_{}", name);
        let untagged =
            format_ident!("__door_server_procedure_untagged_{}", name);
        (
            trampoline(self_ty, door, &dispatch, &tagged, &untagged),
            quote! {
                let __protocol = #p::reply_protocol(&__builder);
                match __protocol {
                    #p::ReplyProtocol::Tagged => {
                        #p::DoorBuilder::build(__builder, #tagged)
                    }
                    #p::ReplyProtocol::Untagged => {
                        #p::DoorBuilder::build(__builder, #untagged)
                    }
                }
            },
        )
    };

    // Why this check is at run time and not at compile time.
    //
    // The macro sees the `impl` block and nothing else. The builder
    // chain is written somewhere else entirely, perhaps in another
    // file, and `.request_size()` returns the same type it took, so
    // there is no trace of the call left in the type for `build_foo`
    // to look at. A compile-time check would need the builder to
    // change type on every method, which would make every ordinary
    // chain harder to read for one rare mistake.
    //
    // So the builder records the call in a flag and we read it here.
    // Silently picking one of the two values is the one thing we must
    // not do: the door would then be built with limits the user never
    // asked for.
    let conflict = {
        let sets_request_size = opts.request_size.is_some();
        let sets_max_descriptors = opts.max_descriptors.is_some();
        if sets_request_size || sets_max_descriptors {
            quote! {
                #p::check_builder_conflicts(
                    &self,
                    #sets_request_size,
                    #sets_max_descriptors,
                )?;
            }
        } else {
            TokenStream::new()
        }
    };

    // Attributes and parameters from `#[door(...)]`. Written as
    // `Type::method(value)` rather than `value.method()` so that a
    // trait the user imported cannot take the call.
    //
    // `DOOR_NO_CANCEL` is not here on purpose: the builder always sets
    // it and there is no option to clear it (`GOALS.md` §12.7).
    let mut setters = Vec::new();

    if opts.refuse_desc {
        setters.push(quote! {
            let __builder = #p::DoorBuilder::refuse_descriptors(__builder);
        });
    }
    if opts.unref {
        setters.push(quote! {
            let __builder = #p::DoorBuilder::unref(__builder);
        });
    }
    if opts.unref_multi {
        setters.push(quote! {
            let __builder = #p::DoorBuilder::unref_multi(__builder);
        });
    }
    if opts.private {
        setters.push(quote! {
            let __builder = #p::DoorBuilder::private_pool(__builder);
        });
    }
    // The flag goes through the builder rather than straight into the
    // generated code, so that `#[door(untagged)]` and
    // `.untagged()` are one setting with one answer, whichever way the
    // user wrote it. Asking twice is harmless: both say the same thing.
    if opts.untagged {
        setters.push(quote! {
            let __builder = #p::DoorBuilder::untagged(__builder);
        });
    }
    if let Some(range) = &opts.request_size {
        let start = match &range.start {
            Some(start) => start.to_token_stream(),
            // `..=8192` means "up to 8192", so the floor is zero.
            None => quote!(0),
        };
        let end = &range.end;
        setters.push(quote! {
            let __builder = #p::DoorBuilder::request_size(
                __builder,
                (#start)..=(#end),
            );
        });
    }
    if let Some(max) = &opts.max_descriptors {
        setters.push(quote! {
            let __builder = #p::DoorBuilder::max_descriptors(
                __builder,
                #max,
            );
        });
    }

    quote! {
        #definition
        #conflict
        let __builder = self;
        #(#setters)*
        #create
    }
}

/// The generated `extern "C"` server procedures.
///
/// Their signature is exactly `door_server_procedure_t`. They do no
/// work of their own: `doors::__private::run` owns the dangerous part,
/// and these functions only hand it a closure. `run` never returns,
/// which is what stops them from falling off their end
/// (`GOALS.md` §4.2 rule 1).
///
/// # Why there are three functions and not one
///
/// A door reply is either tagged or untagged (`GOALS.md` §6.5), and
/// the kernel gives a server procedure five arguments of its own with
/// no room for a sixth of ours. So the answer cannot be handed in at
/// call time. There is one entry point per protocol instead, and
/// `build_foo` registers the one the builder asked for.
///
/// Both call the same third function, so the user's code appears once
/// here and `run` is compiled once. The two entry points hold nothing
/// but the constant that tells them apart.
///
/// # Why they are safe functions
///
/// The entry points are safe `extern "C" fn`s. Rust coerces those to
/// the `unsafe extern "C" fn` the door library wants, and writing them
/// this way means the `unsafe` block around `run` stays visible
/// instead of covering a whole body.
///
/// The shared function is safe for the same reason and one more: all
/// three are items nested inside `build_foo`, so nothing outside that
/// body can name them, and the only caller of the shared one is an
/// entry point the kernel drives.
fn trampoline(
    self_ty: &Type,
    door: &DoorFn<'_>,
    dispatch: &Ident,
    tagged: &Ident,
    untagged: &Ident,
) -> TokenStream {
    let p = private();
    let opts = &door.opts;

    // `refuse_desc` sets DOOR_REFUSE_DESC, so the kernel never
    // delivers a descriptor here. The typestate says the same thing in
    // the type system, which is why no runtime check is generated.
    let policy = if opts.refuse_desc {
        quote!(#p::NoDescriptors)
    } else {
        quote!(#p::Descriptors)
    };

    let error_type = match opts.shape {
        Shape::Rpc | Shape::ReplyBuf => wrapper_error(),
        _ => TokenStream::new(),
    };

    let body = closure_body(self_ty, door);

    quote! {
        #error_type

        // The user's code appears once, here, so `run` is compiled
        // once no matter which protocol the door ends up using.
        fn #dispatch(
            __cookie: *mut #p::c_void,
            __argp: *mut #p::c_char,
            __arg_size: usize,
            __dp: *mut #p::door_desc_t,
            __n_desc: #p::c_uint,
            __protocol: #p::ReplyProtocol,
        ) -> ! {
            // SAFETY: only ever reached because the kernel called one
            // of the two entry points below as a door server
            // procedure, and every argument is passed on exactly as it
            // arrived.
            unsafe {
                #p::run::<#self_ty, #policy, _, _>(
                    __cookie,
                    __argp,
                    __arg_size,
                    __dp,
                    __n_desc,
                    #p::ReplyBuf::DEFAULT_LIMIT,
                    __protocol,
                    |__state, __request| #body,
                )
            }
        }

        extern "C" fn #tagged(
            __cookie: *mut #p::c_void,
            __argp: *mut #p::c_char,
            __arg_size: usize,
            __dp: *mut #p::door_desc_t,
            __n_desc: #p::c_uint,
        ) {
            #dispatch(
                __cookie,
                __argp,
                __arg_size,
                __dp,
                __n_desc,
                #p::ReplyProtocol::Tagged,
            )
        }

        extern "C" fn #untagged(
            __cookie: *mut #p::c_void,
            __argp: *mut #p::c_char,
            __arg_size: usize,
            __dp: *mut #p::door_desc_t,
            __n_desc: #p::c_uint,
        ) {
            #dispatch(
                __cookie,
                __argp,
                __arg_size,
                __dp,
                __n_desc,
                #p::ReplyProtocol::Untagged,
            )
        }
    }
}

/// An error type for the shapes that can fail before the user's code
/// runs.
///
/// The `rpc` shape can fail to decode a request, and the `reply_buf`
/// shape can be handed more bytes than the reply may carry. Neither
/// failure can be reported as the user's own error type `E`, because
/// there is no way to build an `E` out of nothing.
///
/// So the trampoline replies with this instead. It is `Display`, and
/// `doors` implements `ErrorReply` for everything that is `Display`,
/// so it needs no extra support from the library.
///
/// On a tagged door the reply is a §3.9 tag `1` — "the server
/// function returned an error". Tag `2` would describe an
/// infrastructure failure more exactly, but only the trampoline itself
/// can write a tag `2`, and by design a closure cannot.
///
/// On an untagged door there is no tag, so this message goes back as
/// the whole reply and the caller cannot tell it from data. That is
/// the deal an untagged door makes (`GOALS.md` §6.5), and it is why a
/// door serving a foreign peer should say what went wrong in its own
/// reply format.
fn wrapper_error() -> TokenStream {
    quote! {
        enum __DoorError<E> {
            /// The user's own error.
            User(E),
            /// Something went wrong before or after their code ran.
            Message(&'static str),
        }

        impl<E> ::core::fmt::Display for __DoorError<E>
        where
            E: ::core::fmt::Display,
        {
            fn fmt(
                &self,
                __f: &mut ::core::fmt::Formatter<'_>,
            ) -> ::core::fmt::Result {
                match self {
                    __DoorError::User(__e) => {
                        ::core::fmt::Display::fmt(__e, __f)
                    }
                    __DoorError::Message(__m) => __f.write_str(__m),
                }
            }
        }
    }
}

/// The closure `run` calls: one door invocation, in Rust terms.
fn closure_body(self_ty: &Type, door: &DoorFn<'_>) -> TokenStream {
    let p = private();
    let opts = &door.opts;
    let name = &door.method.sig.ident;

    // The unreferenced notification is not a call. It carries no
    // payload, nobody is waiting for the answer, and the request
    // pointer is the literal address 1 rather than data. Only doors
    // that asked for it get this check.
    let unref = if opts.wants_unref() {
        quote! {
            if #p::Request::is_unreferenced(&__request) {
                #self_ty::on_unreferenced(__state);
                return #p::Outcome::bytes(
                    ::core::result::Result::Ok(::std::vec::Vec::new()),
                );
            }
        }
    } else {
        TokenStream::new()
    };

    match opts.shape {
        Shape::Procedure => quote! {
            {
                #unref
                #p::Outcome::bytes(#self_ty::#name(__state, __request))
            }
        },

        // The two `rpc` calls are the only place the wire format is
        // decided, and both live in `doors`. This crate never names
        // `serde` or `postcard` (`GOALS.md` §3.8).
        //
        // The odd alias is deliberate. `doors::__private::rpc` only
        // exists when the `rpc` feature is on, and a macro cannot see
        // another crate's features. When the feature is off the import
        // fails to resolve, and the name is the message.
        Shape::Rpc => quote! {
            {
                use #p::rpc as __door_rpc_needs_the_doors_rpc_feature;
                #unref
                let __body = match
                    __door_rpc_needs_the_doors_rpc_feature::decode_request(
                        #p::Request::data(&__request),
                    )
                {
                    ::core::result::Result::Ok(__body) => __body,
                    ::core::result::Result::Err(_) => {
                        return #p::Outcome::bytes(
                            ::core::result::Result::Err(
                                __DoorError::Message(
                                    "the request could not be decoded",
                                ),
                            ),
                        );
                    }
                };
                match #self_ty::#name(__state, __body) {
                    ::core::result::Result::Ok(__reply) => match
                        __door_rpc_needs_the_doors_rpc_feature::encode_reply(
                            &__reply,
                        )
                    {
                        ::core::result::Result::Ok(__bytes) => {
                            #p::Outcome::bytes(
                                ::core::result::Result::Ok(__bytes),
                            )
                        }
                        ::core::result::Result::Err(_) => {
                            #p::Outcome::bytes(
                                ::core::result::Result::Err(
                                    __DoorError::Message(
                                        "the reply could not be encoded",
                                    ),
                                ),
                            )
                        }
                    },
                    ::core::result::Result::Err(__e) => #p::Outcome::bytes(
                        ::core::result::Result::Err(
                            __DoorError::User(__e),
                        ),
                    ),
                }
            }
        },

        // The user writes into a buffer of ours, not the trampoline's.
        // `run` owns its own `ReplyBuf` and does not lend it out, so
        // the bytes are copied once on the way back. An overflow is
        // reported rather than sent: a truncated reply would decode as
        // nonsense on the client.
        Shape::ReplyBuf => quote! {
            {
                #unref
                let mut __out = #p::ReplyBuf::new();
                match #self_ty::#name(__state, __request, &mut __out) {
                    ::core::result::Result::Ok(()) => {
                        if #p::ReplyBuf::overflow(&__out).is_some() {
                            return #p::Outcome::bytes(
                                ::core::result::Result::Err(
                                    __DoorError::Message(
                                        "the reply did not fit in the \
                                         server's reply buffer",
                                    ),
                                ),
                            );
                        }
                        #p::Outcome::bytes(
                            ::core::result::Result::Ok(
                                #p::ReplyBuf::as_slice(&__out).to_vec(),
                            ),
                        )
                    }
                    ::core::result::Result::Err(__e) => #p::Outcome::bytes(
                        ::core::result::Result::Err(
                            __DoorError::User(__e),
                        ),
                    ),
                }
            }
        },

        // The only shape whose reply can carry descriptors, and so the
        // only one that builds an `Outcome` field by field.
        // `Outcome::bytes` hard-codes an empty list, which is right for
        // every other shape and wrong for this one.
        //
        // The trampoline owns what happens next. It moves the
        // descriptors out of their `OwnedFd`s, sends them, and closes
        // them by hand if `door_return` comes back. Nothing about that
        // is repeated here.
        //
        // An `Err` sends no descriptors, because there are none: the
        // user returned one value in that case, not a pair.
        Shape::Handback => quote! {
            {
                #unref
                match #self_ty::#name(__state, __request) {
                    ::core::result::Result::Ok((__data, __fds)) => {
                        #p::Outcome {
                            data: ::core::result::Result::Ok(__data),
                            descriptors: __fds,
                        }
                    }
                    ::core::result::Result::Err(__e) => #p::Outcome {
                        data: ::core::result::Result::Err(__e),
                        descriptors: ::std::vec::Vec::new(),
                    },
                }
            }
        },

        // No trampoline is generated for `raw`; this is never reached.
        Shape::Raw => TokenStream::new(),
    }
}
