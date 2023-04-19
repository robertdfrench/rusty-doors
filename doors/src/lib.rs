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
use std::os::fd;
use std::os::fd::AsRawFd;

impl AsRawFd for door_h::door_desc_t {
    fn as_raw_fd(&self) -> fd::RawFd {
        let d_data = &self.d_data;
        let d_desc = unsafe { d_data.d_desc };
        let d_descriptor = d_desc.d_descriptor;
        d_descriptor as fd::RawFd
    }
}

impl door_h::door_desc_t {
    /// Create a new `door_desc_t` from a file descriptor.
    ///
    /// When passing a file descriptor through a door call, the kernel needs to
    /// know whether it should *release* that descriptor: that is, should we
    /// transfer exclusive control of the descriptor to the receiving process,
    /// or should each process have independent access to the resource
    /// underlying the descriptor?
    ///
    /// Setting `release` to false means that both the server and the client
    /// will have the same level of access to the underlying resource, and they
    /// must take care not to cause conflicts.
    ///
    /// Setting `release` to true means that the sender will no longer have
    /// access to the resource -- effecively, the file descriptor will be closed
    /// once the `door_call` or `door_return` has completed. In this case, the
    /// recipient will have exclusive control over the resource referenced by
    /// this file descriptor.
    pub fn new(raw: fd::RawFd, release: bool) -> Self {
        let d_descriptor = raw as libc::c_int;
        let d_id = 0;
        let d_desc = door_h::door_desc_t__d_data__d_desc { d_descriptor, d_id };
        let d_data = door_h::door_desc_t__d_data { d_desc };

        let d_attributes = match release {
            false => door_h::DOOR_DESCRIPTOR,
            true => door_h::DOOR_DESCRIPTOR | door_h::DOOR_RELEASE,
        };
        Self {
            d_attributes,
            d_data,
        }
    }

    pub fn will_release(&self) -> bool {
        self.d_attributes == (door_h::DOOR_DESCRIPTOR | door_h::DOOR_RELEASE)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_raw_fd() {
        let dd = door_h::door_desc_t::new(-1, true);
        assert_eq!(dd.as_raw_fd(), -1);
    }

    #[test]
    fn retain_door_desc_t() {
        let dd = door_h::door_desc_t::new(-1, false);
        assert!(!dd.will_release());
    }

    #[test]
    fn release_door_desc_t() {
        let dd = door_h::door_desc_t::new(-1, true);
        assert!(dd.will_release());
    }
}
