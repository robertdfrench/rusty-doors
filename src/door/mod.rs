extern crate libc;
use self::libc::{c_int, c_void, c_char, c_uint, size_t, O_RDWR, O_CREAT, O_EXCL };
use self::libc::open as c_open;
use std::ptr;
use std::os::unix::io::IntoRawFd;
use std::os::unix::io::FromRawFd;
use std::fs::File;
use std::ffi::CString;

mod door_h;
use self::door_h::{
	door_desc_t,
	door_call,
	door_create,
	door_return,
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

extern "C" fn answer(_cookie: *const c_void, argp: *const c_char, arg_size: size_t, dp: *const door_desc_t, n_desc: c_uint) {
	println!("I am answering a door call!");
	unsafe { door_return(argp, arg_size, dp, n_desc) };
	panic!("Door return failed!");
}

pub fn create() -> Option<Door> {
	let fd = unsafe { door_create(answer, ptr::null(), 0) };
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

pub fn create_at(path: &str) -> Option<Door> {
	match create() {
		None => None,
		Some(door) => mount(door, path)
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
