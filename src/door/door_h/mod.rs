/* 
 * Incorporate door definitions from /usr/include/sys/door.h
 */
extern crate libc;
use self::libc::{
	c_uint,
	c_ulonglong,
	c_int,
	c_char,
	size_t,
	c_void,
};

#[allow(non_camel_case_types)]
pub type door_attr_t = c_uint;

#[allow(non_camel_case_types)]
type door_id_t = c_ulonglong;

#[repr(C)]
#[derive(Copy, Clone)]
struct door_desc_t__d_data__d_desc {
	d_descriptor: c_int,
	d_id: door_id_t
}

#[repr(C)]
union door_desc_t__d_data {
	d_desc: door_desc_t__d_data__d_desc,
	d_resv: [c_int; 5] /* Check out /usr/include/sys/door.h */
}

#[repr(C)]
pub struct door_desc_t {
	d_attributes: door_attr_t,
	d_data: door_desc_t__d_data
}

#[repr(C)]
pub struct door_arg_t {
	data_ptr: *const c_char,
	data_size: size_t,
	desc_ptr: *const door_desc_t,
	dec_num: c_uint,
	rbuf: *const c_char,
	rsize: size_t
}

extern {
	pub fn door_call(d: c_int, params: *const door_arg_t) -> c_int;
	pub fn door_create(server_procedure: extern fn(cookie: *const c_void, argp: *const c_char, arg_size: size_t, dp: *const door_desc_t, n_desc: c_uint), cookie: *const c_void, attributes: door_attr_t) -> c_int;
	pub fn door_return(data_ptr: *const c_char, data_size: size_t, desc_ptr: *const door_desc_t, num_desc: c_uint);
}
