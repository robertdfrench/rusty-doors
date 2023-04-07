//! Traits for easier Server Procedures
//!
//! This should be mostly replaced with proc macros one day.

use crate::illumos::door_h::door_create;
use crate::illumos::door_h::door_desc_t;
use crate::illumos::door_h::door_return;
use crate::illumos::door_h::door_server_procedure_t;
use crate::illumos::errno_h::errno;
use crate::illumos::stropts_h::fattach;
use libc;
use std::ffi;
use std::os::fd::AsRawFd;
use std::os::fd::IntoRawFd;
use std::os::fd::RawFd;

/// Door problems.
///
/// Two things can go wrong with a door -- its path can be invalid, or a system
/// call can fail. If a system call fails, one of this enum's variants will be
/// returned corresponding to the failed system call. It will contain the value
/// of `errno` associated with the failed system call.
#[derive(Debug)]
pub enum Error {
    InvalidPath(ffi::NulError),
    InstallJamb(libc::c_int),
    AttachDoor(libc::c_int),
    OpenDoor(std::io::Error),
    DoorCall(libc::c_int),
    CreateDoor(libc::c_int),
}

pub struct Server {
    pub jamb_path: ffi::CString,
    pub door_descriptor: libc::c_int,
}

impl IntoRawFd for Server {
    fn into_raw_fd(self) -> RawFd {
        self.door_descriptor.as_raw_fd()
    }
}

pub struct Request<'a> {
    pub cookie: u64,
    pub data: &'a [u8],
    pub descriptors: &'a [door_desc_t],
}

pub struct Response<'a> {
    pub data: &'a [u8],
    pub num_descriptors: u32,
    pub descriptors: [door_desc_t; 2],
}

impl<'a> Response<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let descriptors =
            [door_desc_t::new(-1, true), door_desc_t::new(-1, true)];
        let num_descriptors = 0;
        Self {
            data,
            descriptors,
            num_descriptors,
        }
    }

    pub fn add_descriptor(mut self, fd: RawFd, release: bool) -> Self {
        if self.num_descriptors == 2 {
            panic!("Only 2 descriptors are supported")
        }

        let desc = door_desc_t::new(fd, release);
        self.descriptors[self.num_descriptors as usize] = desc;
        self.num_descriptors += 1;

        self
    }
}

pub trait ServerProcedure {
    fn server_procedure(payload: Request<'_>) -> Response;

    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    extern "C" fn c_wrapper(
        cookie: *const libc::c_void,
        argp: *const libc::c_char,
        arg_size: libc::size_t,
        dp: *const door_desc_t,
        n_desc: libc::c_uint,
    ) {
        let data = unsafe {
            std::slice::from_raw_parts::<u8>(argp as *const u8, arg_size)
        };
        let descriptors = unsafe {
            std::slice::from_raw_parts(dp, n_desc.try_into().unwrap())
        };
        let cookie = cookie as u64;
        let payload = Request {
            cookie,
            data,
            descriptors,
        };
        let response = Self::server_procedure(payload);
        unsafe {
            door_return(
                response.data.as_ptr() as *const libc::c_char,
                response.data.len(),
                response.descriptors.as_ptr(),
                response.num_descriptors,
            )
        };
    }

    /// Make this procedure available on the filesystem (as a door).
    fn install(cookie: u64, path: &str, attrs: u32) -> Result<Server, Error> {
        install_server_procedure(Self::c_wrapper, cookie, path, attrs)
    }
}

pub trait RawServerProcedure {
    fn server_procedure(cookie: u64, data: &[u8], desc: &[door_desc_t]);

    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    extern "C" fn c_wrapper(
        cookie: *const libc::c_void,
        argp: *const libc::c_char,
        arg_size: libc::size_t,
        dp: *const door_desc_t,
        n_desc: libc::c_uint,
    ) {
        let data = unsafe {
            std::slice::from_raw_parts::<u8>(argp as *const u8, arg_size)
        };
        let desc = unsafe {
            std::slice::from_raw_parts(dp, n_desc.try_into().unwrap())
        };
        Self::server_procedure(cookie as u64, data, desc);
    }

    /// Make this procedure available on the filesystem (as a door).
    fn install(cookie: u64, path: &str, attrs: u32) -> Result<Server, Error> {
        install_server_procedure(Self::c_wrapper, cookie, path, attrs)
    }
}

fn install_server_procedure(
    server_procedure: door_server_procedure_t,
    cookie: u64,
    path: &str,
    attrs: u32,
) -> Result<Server, Error> {
    let jamb_path = ffi::CString::new(path).unwrap();

    // Create door
    let door_descriptor = unsafe {
        door_create(server_procedure, cookie as *const libc::c_void, attrs)
    };
    if door_descriptor == -1 {
        return Err(Error::CreateDoor(errno()));
    }

    // Create jamb
    let create_new = libc::O_RDWR | libc::O_CREAT | libc::O_EXCL;
    match unsafe { libc::open(jamb_path.as_ptr(), create_new, 0o644) } {
        -1 => {
            // Clean up the door, since we aren't going to finish
            unsafe { libc::close(door_descriptor) };
            return Err(Error::InstallJamb(errno()));
        }
        jamb_descriptor => unsafe {
            libc::close(jamb_descriptor);
        },
    }

    // Attach door to jamb
    match unsafe { fattach(door_descriptor, jamb_path.as_ptr()) } {
        -1 => {
            // Clean up the door and jamb, since we aren't going to finish
            unsafe { libc::close(door_descriptor) };
            unsafe {
                libc::unlink(jamb_path.as_ptr());
            }
            Err(Error::AttachDoor(errno()))
        }
        _ => Ok(Server {
            jamb_path,
            door_descriptor,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::door_h;
    use crate::illumos::errno_h;
    use std::ffi::CString;
    use std::io::Read;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::fd::FromRawFd;
    use std::path::Path;
    use std::ptr;

    #[test]
    fn new_door_arg() {
        let text = b"Hello, World!";
        let mut buffer = [0; 1024];
        let args = door_h::door_arg_t::new(text, &vec![], &mut buffer);
        let door =
            std::fs::File::open("/tmp/capitalize_door_response.door").unwrap();
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
    fn fancy_capitalize() {
        let text = b"Hello, World!";
        let mut buffer = [0; 1024];
        let args = door_h::door_arg_t::new(text, &vec![], &mut buffer);
        let door = std::fs::File::open("/tmp/fancy_capitalize.door").unwrap();
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
    fn fancy_receive_file_descriptor() {
        let door_path = Path::new("/tmp/fancy_open_server.door");
        let door_path_cstring =
            CString::new(door_path.to_str().unwrap()).unwrap();

        let txt_path = Path::new("/tmp/fancy_open_server.txt");
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
        let data_size = 26;
        let desc_ptr = ptr::null();
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
