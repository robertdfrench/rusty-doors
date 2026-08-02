/*
 * Does door_revoke(3C) close the descriptor, or only invalidate it?
 *
 * This decides whether the crate may call close(2) afterwards. If
 * door_revoke already closed it, our close is a double close -- and a
 * double close in a threaded program does not merely fail, it shuts
 * whatever descriptor another thread has since been given that number.
 *
 * See docs/DESIGN.md Appendix E.
 *
 *   gcc -m64 -Wall -o revoke_closes revoke_closes.c && ./revoke_closes
 */
#include <door.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

/* ARGSUSED */
static void
proc(void *c, char *a, size_t s, door_desc_t *d, uint_t n)
{
	(void) door_return(NULL, 0, NULL, 0);
}

int
main(void)
{
	int did, rc;

	did = door_create(proc, NULL, DOOR_NO_CANCEL);
	if (did < 0) {
		(void) printf("door_create: %s\n", strerror(errno));
		return (1);
	}
	(void) printf("door_create returned fd %d\n", did);

	errno = 0;
	if (fcntl(did, F_GETFD) == -1) {
		(void) printf("FAIL: fd was not open before revoke\n");
		return (1);
	}
	(void) printf("before revoke: fd %d is open\n", did);

	rc = door_revoke(did);
	(void) printf("door_revoke(%d) = %d\n", did, rc);

	errno = 0;
	if (fcntl(did, F_GETFD) == -1) {
		(void) printf("after revoke:  fd %d is CLOSED (errno %d, %s)\n",
		    did, errno, strerror(errno));
		(void) printf("\nRESULT door_revoke_closes_the_fd=1\n");
		(void) printf("So calling close(%d) now would be a DOUBLE "
		    "CLOSE.\n", did);

		errno = 0;
		rc = close(did);
		(void) printf("close(%d) = %d errno=%d (%s)\n", did, rc,
		    errno, rc == 0 ? "no error" : strerror(errno));
		return (0);
	}

	(void) printf("after revoke:  fd %d is still open\n", did);
	(void) printf("\nRESULT door_revoke_closes_the_fd=0\n");
	(void) close(did);
	return (0);
}
