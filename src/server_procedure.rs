//! Traits for easier Server Procedures

use crate::illumos::door_h;
use crate::illumos::door_h::door_create;
use crate::illumos::door_h::door_desc_t;
use crate::illumos::errno_h::errno;
use crate::illumos::stropts_h::fattach;
use libc;
use std::ffi;
use std::os::fd::AsRawFd;
use std::os::fd::IntoRawFd;
use std::os::fd::RawFd;
use std::ptr;

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

pub trait ServerProcedure: Sized {
    fn server_procedure(&mut self);

    extern "C" fn c_wrapper(
        cookie: *const libc::c_void,
        _argp: *const libc::c_char,
        _arg_size: libc::size_t,
        _dp: *const door_desc_t,
        _n_desc: libc::c_uint,
    ) {
        // let x = [5, 6, 7];
        // let raw_pointer = x.as_ptr();
        // let slice = ptr::slice_from_raw_parts(raw_pointer, 3);
        // assert_eq!(unsafe { &*slice }[2], 7);
        let x = cookie as *mut Self;
        unsafe { (*x).server_procedure() };
        unsafe { door_h::door_return(ptr::null(), 0, ptr::null(), 0) };
    }

    /// Make this procedure available on the filesystem (as a door).
    fn install(&self, path: &str, attrs: u32) -> Result<Server, Error> {
        let jamb_path = ffi::CString::new(path).unwrap();

        // Create door
        let door_descriptor = unsafe {
            door_create(
                Self::c_wrapper,
                ptr::addr_of!(*self) as *const libc::c_void,
                attrs,
            )
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
}

#[cfg(test)]
mod tests {
    use crate::client::Plain;
    use crate::door_h::door_arg_t;

    #[test]
    fn knock_once() {
        let text = b"";
        let mut buffer = [0; 1];
        let mut args = door_arg_t::new(text, &vec![], &mut buffer);
        let door = Plain::open("/tmp/knock_only_server.door").unwrap();

        door.call(&mut args).unwrap();
    }

    #[test]
    fn knock_twice() {
        let text = b"";
        let mut buffer = [0; 1];
        let mut args = door_arg_t::new(text, &vec![], &mut buffer);
        let door = Plain::open("/tmp/knock_only_server.door").unwrap();

        door.call(&mut args).unwrap();
    }
}
