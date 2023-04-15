//! This crate contains a single macro [`macro@server_procedure`] for transforming a rust
//! function into a server procedure.

use proc_macro::TokenStream;
use quote::{format_ident, quote, ToTokens};
use syn::spanned::Spanned;
use syn::{parse_macro_input, Error, FnArg, ItemFn, Pat, ReturnType};

/// This macro transforms function into a door call handler. See `doors` crate
/// documentation for usage.
///
/// Only single argument functions are supported e.g.
/// ```
/// use doors::server::Request;
/// use doors::server::Response;
///
/// #[door_macros::server_procedure]
/// fn serv_proc(x: Request<'_>) -> Response<[u8; 1]> {
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
            "doors should take a single Request as input",
        )
        .to_compile_error()
        .into();
    }

    // extract the single argument and it's type
    let arg = &input.sig.inputs[0];
    let (arg_ident, _arg_type) = match arg {
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
            cookie: *const std::os::raw::c_void,
            argp: *const std::os::raw::c_char,
            arg_size: usize,
            dp: *const doors::illumos::door_h::door_desc_t,
            n_desc: std::os::raw::c_uint,
         ) {

            let f = || -> #return_type {
                let #arg_ident = doors::server::Request {
                    data: unsafe {
                        std::slice::from_raw_parts::<u8>(
                            argp as *const u8,
                            arg_size
                        )
                    },
                    descriptors: unsafe {
                        std::slice::from_raw_parts(
                            dp,
                            n_desc.try_into().unwrap()
                        )
                    },
                    cookie: cookie as u64
                };
                #blk
            };

            let mut response = f();
            match response.data {
                Some(data) => unsafe {
                    doors::illumos::door_h::door_return(
                        data.as_ref().as_ptr() as *const std::os::raw::c_char,
                        data.as_ref().len(),
                        response.descriptors.as_ptr(),
                        response.num_descriptors,
                    )
                },
                None => unsafe {
                    doors::illumos::door_h::door_return(
                        std::ptr::null() as *const std::os::raw::c_char,
                        0,
                        response.descriptors.as_ptr(),
                        response.num_descriptors,
                    )
                }
            }

        }

    };

    TokenStream::from(q)
}
