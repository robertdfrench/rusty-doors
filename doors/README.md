# Rusty Doors
![](https://github.com/robertdfrench/rusty-doors/raw/HEAD/etc/social_media_preview.jpg)

The goal of this crate is to expose the [illumos][1] [Doors API][2] in
Rust. It exposes the native doors API verbatim, and also provides some
moderately safer abstractions.

## What are Doors?
A *door* is a file-like mechanism for interprocess communication, not
unlike a named pipe or a UNIX Domain Socket. Client programs can invoke
functions (called *server procedures*) in door servers if the door
server has made a door available on the filesystem.

A *server procedure* is a function within the door server program that
has a special, predefined signature. It is the entrypoint for the thread
that is created (or awoken) to handle a client's door invocation.

A *door server* is a process that has created a *door* from one of its
*server procedures*.

## Example

A *server procedure* that simply doubles its input might look like this:

```rust
use doors::server::Door;
use doors::server::Request;
use doors::server::Response;

#[doors::server_procedure]
fn double(x: Request) -> Response<[u8; 1]> {
  if x.data.len() > 0 {
    return Response::new([x.data[0] * 2]);
  } else {
    // We were given nothing, and 2 times nothing is zero...
    return Response::new([0]);
  }
}

let door = Door::create(double).unwrap();
door.force_install("/tmp/double.door").unwrap();
```

A client program which invokes that server procedure might look
something like this:

```rust
use doors::client::Client;

let client = Client::open("/tmp/double.door").unwrap();

let response = client.call_with_data(&[111]).unwrap();
assert_eq!(response.data()[0], 222);
```

## Acknowledgements
* The social media preview image is due to [Jim Choate][4] under the
  terms of [CC BY-NC 2.0][5].
* This work preceeds, but was reignited by
  [oxidecomputer/rusty-doors][3].


<!-- REFERENCES -->
[1]: https://illumos.org/
[2]: https://github.com/robertdfrench/revolving-door
[3]: https://github.com/oxidecomputer/rusty-doors
[4]: https://www.flickr.com/photos/jimchoate/50854146398
[5]: https://creativecommons.org/licenses/by-nc/2.0/
