/*
 * A door client that has never heard of this crate.
 *
 * Plain C, plain door_call, and it expects the reply to be exactly the
 * bytes the server produced -- no status byte to skip.
 * doors/tests/interop.rs builds this and points it at an untagged Rust
 * door, which is the other half of proving interop.
 *
 *   gcc -m64 -Wall -o c_client c_client.c
 *   ./c_client /tmp/some_door hello
 */
#include <door.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

int
main(int argc, char **argv)
{
	door_arg_t arg;
	char rbuf[4096];
	int fd;

	if (argc < 3) {
		(void) fprintf(stderr, "usage: %s <door-path> <text>\n",
		    argv[0]);
		return (2);
	}

	fd = open(argv[1], O_RDONLY);
	if (fd < 0) {
		(void) fprintf(stderr, "open %s: %s\n", argv[1],
		    strerror(errno));
		return (1);
	}

	(void) memset(&arg, 0, sizeof (arg));
	arg.data_ptr = argv[2];
	arg.data_size = strlen(argv[2]);
	arg.rbuf = rbuf;
	arg.rsize = sizeof (rbuf);

	if (door_call(fd, &arg) < 0) {
		(void) fprintf(stderr, "door_call: %s\n", strerror(errno));
		return (1);
	}

	/* Write the reply verbatim, so the test can assert on it. */
	(void) fwrite(arg.data_ptr, 1, arg.data_size, stdout);
	(void) fflush(stdout);

	/*
	 * If the reply did not fit in rbuf the kernel mapped fresh pages
	 * for it, and they are ours to release.
	 */
	if (arg.rbuf != rbuf)
		(void) munmap(arg.rbuf, arg.rsize);

	return (0);
}
