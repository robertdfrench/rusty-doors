extern crate libc;
use self::libc::{c_int, O_RDWR, O_CREAT, O_EXCL };
use self::libc::{
	c_void,
	c_char,
	size_t,
	c_uint,
};
use self::libc::open as c_open;
use std::ptr;
use std::os::unix::io::IntoRawFd;
use std::os::unix::io::FromRawFd;
use std::fs::File;
use std::ffi::CString;

pub mod door_h;
use self::door_h::{
	door_call,
	door_create,
	door_desc_t,
	door_return,
	door_server_proc_t
};

mod stropts_h;
use self::stropts_h::fattach;

pub struct Door {
	descriptor: c_int
}

pub fn from(file: File) -> Door {
	Door { descriptor: file.into_raw_fd() }
}

pub fn server_safe_open(path: &str) -> Option<File> {
	let cpath = CString::new(path).unwrap();
	let fd = unsafe { c_open(cpath.as_ptr(), O_RDWR|O_CREAT|O_EXCL, 0400) };
	if fd < 0 {
		None
	} else {
		let file = unsafe { File::from_raw_fd(fd) };
		Some(file)
	}
}

pub trait ServerProcedure {
	fn rust();
	extern "C" fn c(
		_cookie: *const c_void, argp: *const c_char, arg_size: size_t,
		dp: *const door_desc_t, n_desc: c_uint
	) {
		Self::rust();
		unsafe { door_return(argp, arg_size, dp, n_desc) };
		panic!("Door return failed!");
	}
	fn attach_to(path: &str) -> Option<Door> {
		match create(Self::c) {
			None => None,
			Some(door) => {
				match server_safe_open(path) {
					None => None,
					Some(_d) => mount(door, path)
				}
			}
		}
	}
}

#[macro_export]
macro_rules! doorfn {
	($i:ident() $b:block) => {
		use door::ServerProcedure;
		struct $i;
		impl ServerProcedure for $i {
			fn rust() $b
		}
	}
}

pub fn create(server_proc: door_server_proc_t) -> Option<Door> {
	let fd = unsafe { door_create(server_proc, ptr::null(), 0) };
	if fd < 0 {
		None
	} else {
		Some(Door{ descriptor: fd })
	}
}

pub fn mount(door: Door, path: &str) -> Option<Door> {
	let cpath = CString::new(path).unwrap();
	let success = unsafe { fattach(door.descriptor, cpath.as_ptr()) };
	if success < 0 {
		None
	} else {
		Some(door)
	}
}
	

impl Door {
	pub fn call(&self) -> bool {
		let success = unsafe { door_call(self.descriptor, ptr::null()) };
		(success >= 0)
	}
}

impl Drop for Door {
	fn drop(&mut self) {
		let fd = self.descriptor;
		unsafe { File::from_raw_fd(fd) };
	}
}
