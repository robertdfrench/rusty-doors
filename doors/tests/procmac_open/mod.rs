use doors::illumos::door_h;
use std::ffi::CString;
use std::io::Read;
use std::io::Write;
use std::os::fd::FromRawFd;
use std::os::fd::RawFd;
use std::path::Path;

#[test]
fn can_receive_file_descriptor() {
    let door_path = Path::new("/tmp/procmac_open_server.door");
    let door_path_cstring = CString::new(door_path.to_str().unwrap()).unwrap();

    let txt_path = Path::new("/tmp/procmac_open_server.txt");
    let mut txt = std::fs::File::create(txt_path).expect("create txt");
    writeln!(txt, "Hello, World!").expect("write txt");
    drop(txt);
    let txt_path_cstring = CString::new(txt_path.to_str().unwrap()).unwrap();

    // Connect to the Capitalization Server through its door.
    let client_door_fd =
        unsafe { libc::open(door_path_cstring.as_ptr(), libc::O_RDONLY) };

    // Pass `original` through the Capitalization Server's door.
    let data_ptr = txt_path_cstring.as_ptr();
    let data_size = 29;
    let desc_ptr = std::ptr::null();
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
