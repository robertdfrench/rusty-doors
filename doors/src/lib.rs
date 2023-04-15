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
    use client;
    use illumos::errno_h;
    use std::ffi::CString;
    use std::io::Read;
    use std::io::Write;
    use std::os::fd::FromRawFd;
    use std::os::fd::RawFd;
    use std::path::Path;

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

    #[test]
    fn new_door_arg() {
        let text = b"Hello, World!";
        let mut buffer = [0; 1024];
        let args = door_h::door_arg_t::new(text, &vec![], &mut buffer);
        let door = std::fs::File::open("/tmp/barebones_server.door").unwrap();
        let door = door.as_raw_fd();

        let rc = unsafe { door_h::door_call(door, &args) };
        if rc == -1 {
            assert_ne!(errno_h::errno(), libc::EBADF);
        }
        assert_eq!(rc, 0);
        assert_eq!(args.data_size, 13);
        let response = unsafe { std::ffi::CStr::from_ptr(args.data_ptr) };
        let response = response.to_str().unwrap();
        assert_eq!(response, "HELLO, WORLD!");
    }

    #[test]
    fn increment_shared_counter() {
        let increment =
            client::Client::open("/tmp/key_value_store_server.door").unwrap();
        let fetch =
            client::Client::open("/tmp/key_value_store_server_fetch.door")
                .unwrap();

        let mut rbuf: [u8; 1] = [0];

        let mut arg = crate::door_h::door_arg_t::new(&[], &[], &mut rbuf);
        increment.call(&mut arg).unwrap();
        increment.call(&mut arg).unwrap();
        increment.call(&mut arg).unwrap();
        fetch.call(&mut arg).unwrap();
        assert_eq!(rbuf[0], 3);
    }

    #[test]
    fn procedural_macro_double_u8() {
        let double = client::Client::open("/tmp/procmac_double.door").unwrap();

        let mut rbuf: [u8; 1] = [0];

        let mut arg = crate::door_h::door_arg_t::new(&[111], &[], &mut rbuf);
        double.call(&mut arg).unwrap();
        assert_eq!(rbuf[0], 222);
    }

    #[test]
    fn procmac_increment_shared_counter() {
        let increment =
            client::Client::open("/tmp/procmac_kv_store.door").unwrap();
        let fetch = client::Client::open("/tmp/procmac_kv_fetch.door").unwrap();

        let mut rbuf: [u8; 1] = [0];

        let mut arg = crate::door_h::door_arg_t::new(&[], &[], &mut rbuf);
        increment.call(&mut arg).unwrap();
        increment.call(&mut arg).unwrap();
        increment.call(&mut arg).unwrap();
        increment.call(&mut arg).unwrap();
        fetch.call(&mut arg).unwrap();
        assert_eq!(rbuf[0], 4);
    }

    #[test]
    fn can_receive_file_descriptor() {
        let door_path = Path::new("/tmp/procmac_open_server.door");
        let door_path_cstring =
            CString::new(door_path.to_str().unwrap()).unwrap();

        let txt_path = Path::new("/tmp/procmac_open_server.txt");
        let mut txt = std::fs::File::create(txt_path).expect("create txt");
        writeln!(txt, "Hello, World!").expect("write txt");
        drop(txt);
        let txt_path_cstring =
            CString::new(txt_path.to_str().unwrap()).unwrap();

        // Connect to the Capitalization Server through its door.
        let client_door_fd =
            unsafe { libc::open(door_path_cstring.as_ptr(), libc::O_RDONLY) };

        // Pass `original` through the Capitalization Server's door.
        let data_ptr = txt_path_cstring.as_ptr();
        let data_size = 29;
        let desc_ptr = std::ptr::null();
        let desc_num = 0;
        let rbuf = unsafe { libc::malloc(data_size) as *mut libc::c_char };
        let rsize = data_size;

        let params = door_h::door_arg_t {
            data_ptr,
            data_size,
            desc_ptr,
            desc_num,
            rbuf,
            rsize,
        };

        // This is where the magic happens. We block here while control is
        // transferred to a separate thread which executes
        // `capitalize_string` on our behalf.
        unsafe { door_h::door_call(client_door_fd, &params) };

        // Unpack the returned bytes and compare!
        let door_desc_ts = unsafe {
            std::slice::from_raw_parts::<door_h::door_desc_t>(
                params.desc_ptr,
                params.desc_num.try_into().unwrap(),
            )
        };
        assert_eq!(door_desc_ts.len(), 1);

        let d_data = &door_desc_ts[0].d_data;
        let d_desc = unsafe { d_data.d_desc };
        let raw_fd = d_desc.d_descriptor as RawFd;
        let mut txt = unsafe { std::fs::File::from_raw_fd(raw_fd) };
        let mut buffer = String::new();
        txt.read_to_string(&mut buffer).expect("read txt");
        assert_eq!(&buffer, "Hello, World!\n");

        // We did a naughty and called malloc, so we need to clean up. A PR
        // for a Rustier way to do this would be considered a personal
        // favor.
        unsafe { libc::free(rbuf as *mut libc::c_void) };
    }
}
