//! This module contains a single macro [`macro@server_procedure`] for transforming a rust
//! function into a server procedure.

use proc_macro::TokenStream;
use quote::{format_ident, quote, ToTokens};
use syn::spanned::Spanned;
use syn::{parse_macro_input, Error, FnArg, ItemFn, Pat, ReturnType};

/// This macro transforms function into a door call handler. See `rusty_doors`
/// module documentation for usage.
///
/// Only single argument functions are supported e.g.
/// ```
/// struct MyResult {}
///
/// #[door_macros::server_procedure]
/// fn serv_proc(x: &[u8]) -> MyResult {
///     todo!();
/// }
/// ```
#[proc_macro_attribute]
pub fn server_procedure(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // parse the function this attribute was applied to
    let input = parse_macro_input!(item as ItemFn);

    // extract the function name
    let name = format_ident!("{}", input.sig.ident.to_string());

    // check number of arguments, we only support a single argument
    if input.sig.inputs.len() != 1 {
        return Error::new(
            input.sig.inputs.span(),
            "only single argument doors supported",
        )
        .to_compile_error()
        .into();
    }

    // extract the single argument and it's type
    let arg = &input.sig.inputs[0];
    let (arg_ident, arg_type) = match arg {
        FnArg::Receiver(_) => {
            return Error::new(
                arg.span(),
                "only standalone functions supported",
            )
            .to_compile_error()
            .into();
        }

        FnArg::Typed(pt) => {
            let p = match &*pt.pat {
                Pat::Ident(i) => i.ident.to_string(),

                _ => {
                    return Error::new(
                        arg.span(),
                        "only identifier arguments supported",
                    )
                    .to_compile_error()
                    .into()
                }
            };
            (format_ident!("{}", p), *pt.ty.clone())
        }
    };

    //extract the return type
    let return_type = match input.sig.output {
        ReturnType::Default => ReturnType::Default.to_token_stream(),
        ReturnType::Type(_, t) => (*t).to_token_stream(),
    };

    // extract the body of the function
    let blk = input.block;

    // generate the output function
    let q = quote! {

        extern "C" fn #name(
            _cookie: *const std::os::raw::c_void,
            dataptr: *const std::os::raw::c_char,
            _datasize: usize,
            _descptr: *const doors::illumos::door_h::door_desc_t,
            _ndesc: std::os::raw::c_uint,
         ) {

            let f = || -> #return_type {
                let #arg_ident = unsafe { *(dataptr as *mut #arg_type) };
                #blk
            };

            let mut result = f();
            unsafe { doors::illumos::door_h::door_return(
                (&mut result as *mut #return_type) as *mut std::os::raw::c_char,
                std::mem::size_of::<#return_type>(),
                std::ptr::null_mut(),
                0,
            ) };

        }

    };

    TokenStream::from(q)
}
