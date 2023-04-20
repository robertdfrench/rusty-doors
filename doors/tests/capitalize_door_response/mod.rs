use doors::illumos::door_h;
use doors::illumos::errno_h;
use doors::illumos::DoorArg;
use std::ffi::CString;
use std::os::fd::AsRawFd;

#[test]
fn new_door_arg() {
    let source = CString::new("Hello, World!").unwrap();
    let text = source.to_bytes_with_nul();
    let mut buffer = [0; 1024];
    let args = DoorArg::new(text, &vec![], &mut buffer);
    let door =
        std::fs::File::open("/tmp/capitalize_door_response.door").unwrap();
    let door = door.as_raw_fd();

    let rc = unsafe { door_h::door_call(door, args.as_door_arg_t()) };
    if rc == -1 {
        assert_ne!(errno_h::errno(), libc::EBADF);
    }
    assert_eq!(rc, 0);
    assert_eq!(args.data().len(), 14);
    let response = std::ffi::CStr::from_bytes_with_nul(args.data()).unwrap();
    let response = response.to_str().unwrap();
    assert_eq!(response, "HELLO, WORLD!");
}
