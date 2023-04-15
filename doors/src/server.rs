//! Traits for easier Server Procedures

use crate::illumos;
use crate::illumos::door_h::door_desc_t;
use crate::illumos::door_h::door_return;
use crate::illumos::fattach;
use libc;
use std::ffi;
use std::fs::File;
use std::io;
use std::os::fd::RawFd;
use std::path::Path;

/// Door problems.
///
/// Two things can go wrong with a door -- its path can be invalid, or a system
/// call can fail. If a system call fails, one of this enum's variants will be
/// returned corresponding to the failed system call. It will contain the value
/// of `errno` associated with the failed system call.
#[derive(Debug)]
pub enum Error {
    InvalidPath(ffi::NulError),
    InstallJamb(std::io::Error),
    AttachDoor(illumos::Error),
    OpenDoor(std::io::Error),
    DoorCall(libc::c_int),
    CreateDoor(illumos::Error),
}

/// A Descriptor for the Door Server
///
/// When a door is created, the kernel hands us back a reference to it by giving
/// us an index in our descriptor table. This is true even if the door hasn't
/// been attached to the filesystem yet, a la pipes or sockets.
pub struct Door(RawFd);

impl Door {
    /// Make this door server available on the filesystem.
    ///
    /// This is necessary if we want other processes to be able to find and call
    /// this door server.
    pub fn install<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        // Create jamb
        let _jamb = match create_new_file(&path) {
            Ok(file) => file,
            Err(e) => return Err(Error::InstallJamb(e)),
        };

        // Attach door to jamb
        match fattach(self.0, &path) {
            Err(e) => {
                // Clean up the jamb, since we aren't going to finish
                std::fs::remove_file(&path).ok();
                Err(Error::AttachDoor(e))
            }
            Ok(()) => Ok(()),
        }
    }
}

impl Drop for Door {
    fn drop(&mut self) {
        unsafe {
            illumos::door_h::door_revoke(self.0);
        }
    }
}

/// Server-Side representation of the client's door arguments
///
/// This type allows us to write server procedures that accept a single argument
/// rather than five separate arguments.
#[derive(Copy, Clone)]
pub struct Request<'a> {
    pub cookie: u64,
    pub data: &'a [u8],
    pub descriptors: &'a [door_desc_t],
}

/// Server-Side representation of the client's door results
///
/// This type can refer to either memory on the stack (which will be cleaned up
/// automatically when [`door_return`] is called) or memory on the heap (which
/// will not). If you return an object that refers to memory on the heap, it is
/// your responsibility to free it later.
///
/// Many door servers allocate a per-thread response area so that each thread
/// can re-use this area for every door invocation assigned to it. That way the
/// memory leaked is constant. Typically, applications that take this approach
/// will free these per-thread response areas when the DOOR_UNREF message is
/// sent.
pub struct Response<C: AsRef<[u8]>> {
    pub data: Option<C>,
    pub num_descriptors: u32,
    pub descriptors: [door_desc_t; 2],
}

impl<C: AsRef<[u8]>> Response<C> {
    pub fn new(data: C) -> Self {
        let descriptors =
            [door_desc_t::new(-1, true), door_desc_t::new(-1, true)];
        let num_descriptors = 0;
        Self {
            data: Some(data),
            descriptors,
            num_descriptors,
        }
    }

    pub fn empty() -> Self {
        let data = None;
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

pub trait ServerProcedure<C: AsRef<[u8]>> {
    fn server_procedure(payload: Request<'_>) -> Response<C>;

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
        match response.data {
            Some(data) => unsafe {
                door_return(
                    data.as_ref().as_ptr() as *const libc::c_char,
                    data.as_ref().len(),
                    response.descriptors.as_ptr(),
                    response.num_descriptors,
                )
            },
            None => unsafe {
                door_return(
                    std::ptr::null() as *const libc::c_char,
                    0,
                    response.descriptors.as_ptr(),
                    response.num_descriptors,
                )
            },
        }
    }

    fn create_server_with_cookie_and_attributes(
        cookie: u64,
        attrs: illumos::DoorAttributes,
    ) -> Result<Door, Error> {
        match illumos::door_create(Self::c_wrapper, cookie, attrs) {
            Ok(fd) => Ok(Door(fd as RawFd)),
            Err(e) => Err(Error::CreateDoor(e)),
        }
    }

    fn create_server_with_cookie(cookie: u64) -> Result<Door, Error> {
        Self::create_server_with_cookie_and_attributes(
            cookie,
            illumos::DoorAttributes::none(),
        )
    }

    fn create_server_with_attributes(
        attrs: illumos::DoorAttributes,
    ) -> Result<Door, Error> {
        Self::create_server_with_cookie_and_attributes(0, attrs)
    }

    fn create_server() -> Result<Door, Error> {
        Self::create_server_with_cookie_and_attributes(
            0,
            illumos::DoorAttributes::none(),
        )
    }
}

fn create_new_file<P: AsRef<Path>>(path: P) -> io::Result<File> {
    File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
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

    #[test]
    #[should_panic]
    fn create_new_fails_if_file_exists() {
        match File::create("/tmp/create_new_fail.txt") {
            // If we can't create the "original" file, we want the test to fail,
            // which means that we *don't* want to panic.
            Err(e) => {
                eprintln!("{:?}", e);
                assert!(true)
            }
            Ok(_file) => {
                create_new_file("/tmp/create_new_fail.txt").unwrap();
            }
        }
    }
}
