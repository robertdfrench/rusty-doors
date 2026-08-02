/*
 * When door_call fails with ENOTSUP, does the kernel take the
 * descriptor or leave it?
 *
 * GOALS.md 6.4 says: EFAULT and EBADF hand the descriptors back, every
 * other errno consumed them. `classify_call_failure` implements that,
 * and its comment says guessing generously would cause double closes.
 *
 * Sending a descriptor to a DOOR_REFUSE_DESC door gives ENOTSUP, which
 * that rule treats as consumed. If the kernel did NOT take it, the
 * crate forgets a live descriptor on every such call -- a leak, once
 * per call.
 *
 * One F_GETFD is not enough to answer this: a descriptor NUMBER can be
 * closed and handed straight back out. So compare file identity,
 * st_dev/st_ino/st_rdev, exactly as matrix.c does.
 *
 *   gcc -m64 -Wall -o refuse_desc_errno refuse_desc_errno.c
 */
#include <door.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static const char *PATH = "/tmp/door_refuse_errno";

/* ARGSUSED */
static void
proc(void *c, char *a, size_t s, door_desc_t *d, uint_t n)
{
	(void) door_return(NULL, 0, NULL, 0);
}

static void
try_one(const char *label, uint_t attrs)
{
	struct stat before, after;
	door_desc_t desc;
	door_arg_t arg;
	int did, fd, cfd, saved;

	(void) fdetach(PATH);
	(void) unlink(PATH);
	(void) close(creat(PATH, 0600));

	did = door_create(proc, NULL, attrs);
	if (did < 0 || fattach(did, PATH) < 0) {
		(void) printf("%-16s setup failed: %s\n", label,
		    strerror(errno));
		return;
	}

	cfd = open(PATH, O_RDONLY);
	fd = open("/dev/null", O_RDONLY);
	if (cfd < 0 || fd < 0 || fstat(fd, &before) != 0) {
		(void) printf("%-16s open failed\n", label);
		return;
	}

	(void) memset(&desc, 0, sizeof (desc));
	desc.d_attributes = DOOR_DESCRIPTOR | DOOR_RELEASE;
	desc.d_data.d_desc.d_descriptor = fd;

	(void) memset(&arg, 0, sizeof (arg));
	arg.desc_ptr = &desc;
	arg.desc_num = 1;

	errno = 0;
	if (door_call(cfd, &arg) == 0) {
		(void) printf("%-16s door_call SUCCEEDED\n", label);
		goto out;
	}
	saved = errno;

	(void) printf("%-16s errno=%-3d (%-22s) ", label, saved,
	    strerror(saved));

	errno = 0;
	if (fcntl(fd, F_GETFD) == -1) {
		(void) printf("fd CONSUMED\n");
	} else if (fstat(fd, &after) == 0 &&
	    before.st_dev == after.st_dev &&
	    before.st_ino == after.st_ino &&
	    before.st_rdev == after.st_rdev) {
		(void) printf("fd SURVIVED, same file  <-- must be handed back\n");
	} else {
		(void) printf("fd number reused by another file\n");
	}

out:
	(void) door_revoke(did);
	(void) fdetach(PATH);
	(void) unlink(PATH);
}

int
main(void)
{
	setvbuf(stdout, NULL, _IONBF, 0);
	(void) printf("Sending one DOOR_RELEASE descriptor to each door:\n\n");
	try_one("REFUSE_DESC", DOOR_REFUSE_DESC | DOOR_NO_CANCEL);
	try_one("plain", DOOR_NO_CANCEL);
	return (0);
}
