extern crate libc;
use self::libc::{c_int, c_char};

extern {
	pub fn fattach(fildes: c_int, path: *const c_char) -> c_int;
}
