use doors::client;
use doors::illumos::DoorArg;

#[test]
fn procedural_macro_double_u8() {
    let double = client::Client::open("/tmp/procmac_double.door").unwrap();

    let mut rbuf: [u8; 1] = [0];

    let mut arg = DoorArg::new(&[111], &[], &mut rbuf);
    double.call(arg.as_mut_door_arg_t()).unwrap();
    assert_eq!(rbuf[0], 222);
}
