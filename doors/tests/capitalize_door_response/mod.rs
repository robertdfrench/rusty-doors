use doors::illumos::door_h;
use doors::illumos::errno_h;
use std::os::fd::AsRawFd;

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
