// A Client<NoDescriptors> has no way to send a descriptor. This is the
// central claim of GOALS.md §6.1: refusing is not a runtime check that
// somebody can forget, it is the absence of a method.
fn main() {}

fn send(
    client: &doors::Client<doors::NoDescriptors>,
    fds: Vec<doors::SentFd<'_>>,
) {
    let _ = client.call_with_descriptors(b"payload", fds);
}
