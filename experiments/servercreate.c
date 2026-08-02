/*
 * door_create + door_server_create: the documented way to control
 * door server threads, including their stack size.
 *
 * door_xcreate is the other way, and on this platform it either
 * crashes inside libc or returns EINVAL (see xcreate3.c). This checks
 * that the classic mechanism does what the crate needs:
 *   - a server thread with a stack size we chose
 *   - cancellation disabled on it
 *   - a working call
 */
#include <door.h>
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <thread.h>
#include <unistd.h>

#define	STACK	(256 * 1024)

static size_t observed_stack;

/* ARGSUSED */
static void *
server_thread(void *arg)
{
	stack_t ss;

	(void) pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, NULL);
	if (thr_stksegment(&ss) == 0)
		observed_stack = ss.ss_size;

	(void) door_return(NULL, 0, NULL, 0);
	return (NULL);
}

/* ARGSUSED */
static void
create_proc(door_info_t *info)
{
	pthread_attr_t attr;
	pthread_t t;

	(void) pthread_attr_init(&attr);
	(void) pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
	(void) pthread_attr_setstacksize(&attr, STACK);
	(void) pthread_create(&t, &attr, server_thread, NULL);
	(void) pthread_attr_destroy(&attr);
}

/* ARGSUSED */
static void
proc(void *cookie, char *argp, size_t sz, door_desc_t *dp, uint_t n)
{
	char reply[64];
	int len;

	len = snprintf(reply, sizeof (reply), "stack=%zu", observed_stack);
	(void) door_return(reply, len, NULL, 0);
}

int
main(void)
{
	door_arg_t arg;
	char rbuf[128];
	int d;

	setvbuf(stdout, NULL, _IONBF, 0);

	(void) door_server_create(create_proc);

	d = door_create(proc, NULL, DOOR_NO_CANCEL | DOOR_REFUSE_DESC);
	if (d < 0) {
		printf("door_create FAILED: %s\n", strerror(errno));
		return (1);
	}
	printf("door_create OK fd=%d\n", d);

	(void) memset(&arg, 0, sizeof (arg));
	arg.rbuf = rbuf;
	arg.rsize = sizeof (rbuf);
	if (door_call(d, &arg) < 0) {
		printf("door_call FAILED: %s\n", strerror(errno));
		return (1);
	}
	printf("door_call OK, reply: %.*s\n", (int)arg.data_size,
	    arg.data_ptr);
	printf("we asked for stack=%d\n", STACK);

	(void) door_revoke(d);
	return (0);
}
