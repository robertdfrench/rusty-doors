use doors::Client;
use doors::DoorPayload;

#[test]
fn can_munmap() {
    let mut p = DoorPayload::new(&[]);

    let junk = Client::open("/tmp/junk.door").unwrap();
    p.call(junk).unwrap();
    assert_eq!(p.data.len(), 4096);
}
