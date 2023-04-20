//! A Rust-friendly interface for [illumos Doors][1].
//!
//! [Doors][2] are a high-speed, RPC-style interprocess communication facility
//! for the [illumos][3] operating system. They enable rapid dialogue between
//! client and server without giving up the CPU timeslice, and are an excellent
//! alternative to pipes or UNIX domain sockets in situations where IPC latency
//! matters.
//!
//! This crate makes it easier to interact with the Doors API from Rust. It can
//! help you create clients, define server procedures, and open or create doors
//! on the filesystem.
//!
//! ## Example
//! ```
//! // In the Server --------------------------------------- //
//! use doors::server::Door;
//! use doors::server::Request;
//! use doors::server::Response;
//!
//! #[doors::server_procedure]
//! fn double(x: Request<'_>) -> Response<[u8; 1]> {
//!   if x.data.len() > 0 {
//!     return Response::new([x.data[0] * 2]);
//!   } else {
//!     // We were given nothing, and 2 times nothing is zero...
//!     return Response::new([0]);
//!   }
//! }
//!
//! let door = Door::create(double).unwrap();
//! door.force_install("/tmp/double.door").unwrap();
//!
//! // In the Client --------------------------------------- //
//! use doors::client::Client;
//! use doors::illumos::door_h;
//!
//! let client = Client::open("/tmp/double.door").unwrap();
//!
//! let mut rbuf: [u8; 1] = [0];
//! let mut arg = door_h::door_arg_t::new(&[111], &[], &mut rbuf);
//!
//! client.call(&mut arg).unwrap();
//! assert_eq!(rbuf[0], 222);
//! ```
//!
//! [1]: https://github.com/robertdfrench/revolving-doors
//! [2]: https://illumos.org/man/3C/door_create
//! [3]: https://illumos.org
pub use door_macros::server_procedure;

pub mod client;
pub mod illumos;
pub mod server;

use illumos::door_h;

impl<'data, 'descriptors, 'response> door_h::door_arg_t {
    pub fn new(
        data: &'data [u8],
        descriptors: &'descriptors [door_h::door_desc_t],
        response: &'response mut [u8],
    ) -> Self {
        let data_ptr = data.as_ptr() as *const libc::c_char;
        let data_size = data.len() as libc::size_t;
        let desc_ptr = descriptors.as_ptr();
        let desc_num = descriptors.len() as libc::c_uint;
        let rbuf = response.as_ptr() as *const libc::c_char;
        let rsize = response.len() as libc::size_t;
        Self {
            data_ptr,
            data_size,
            desc_ptr,
            desc_num,
            rbuf,
            rsize,
        }
    }
}
