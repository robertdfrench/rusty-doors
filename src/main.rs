extern crate libc;
use libc::{c_int, O_RDONLY};
use libc::open;
use std::ffi::CString;

extern {
	fn door_call(d: c_int, params: c_int) -> c_int;
}

fn main() {
	let path = CString::new("/root/revolving-door/40_knock_knock/server.door").unwrap();
	let door = unsafe { open(path.as_ptr(), O_RDONLY) };

	if door < 0 {
		panic!("Could not open door");
	}
	
	let work = unsafe { door_call(door, 0) };
	println!("Door call results: {}", work);
}

