/*
 * The door_xcreate thread-creation contract.
 *
 * The core dump showed door_xcreate_startf dereferencing garbage, with
 * its argument pointing into the *caller's* stack frame. Hypothesis:
 * the create function must not return until the new thread has picked
 * that argument up, because door_xcreate's frame dies when it returns.
 *
 *   ./xcreate3 0   create the thread and return immediately (the naive way)
 *   ./xcreate3 1   create the thread and wait until it has started
 *   ./xcreate3 2   refuse to create a thread at all (return -1)
 */
#include <door.h>
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int mode;

static pthread_mutex_t mtx = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cv = PTHREAD_COND_INITIALIZER;
static int started;

struct relay {
	void *(*f)(void *);
	void *arg;
};

static void *
shim(void *p)
{
	struct relay *r = p;
	void *(*f)(void *) = r->f;
	void *arg = r->arg;

	/* Tell the creator we have copied what we need. */
	(void) pthread_mutex_lock(&mtx);
	started = 1;
	(void) pthread_cond_signal(&cv);
	(void) pthread_mutex_unlock(&mtx);

	return (f(arg));
}

/* ARGSUSED */
static void
proc(void *c, char *a, size_t s, door_desc_t *d, uint_t n)
{
	(void) door_return(NULL, 0, NULL, 0);
}

/* ARGSUSED */
static int
create_thread(door_info_t *info, void *(*f)(void *), void *arg, void *ck)
{
	pthread_attr_t attr;
	pthread_t t;
	static struct relay r;

	if (mode == 2)
		return (-1);

	(void) pthread_attr_init(&attr);
	(void) pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
	(void) pthread_attr_setstacksize(&attr, 256 * 1024);

	if (mode == 0) {
		if (pthread_create(&t, &attr, f, arg) != 0)
			return (-1);
		return (0);
	}

	/* mode 1: hand off through a shim and wait for the pickup. */
	r.f = f;
	r.arg = arg;
	started = 0;
	if (pthread_create(&t, &attr, shim, &r) != 0)
		return (-1);

	(void) pthread_mutex_lock(&mtx);
	while (!started)
		(void) pthread_cond_wait(&cv, &mtx);
	(void) pthread_mutex_unlock(&mtx);
	return (0);
}

int
main(int argc, char **argv)
{
	door_arg_t arg;
	int d;

	setvbuf(stdout, NULL, _IONBF, 0);
	mode = (argc > 1) ? atoi(argv[1]) : 0;
	printf("mode %d ... ", mode);

	errno = 0;
	d = door_xcreate(proc, NULL, DOOR_PRIVATE | DOOR_NO_CANCEL,
	    create_thread, NULL, NULL, 1);
	if (d < 0) {
		printf("door_xcreate FAILED errno=%d (%s)\n",
		    errno, strerror(errno));
		return (1);
	}
	printf("created fd=%d ... ", d);

	(void) memset(&arg, 0, sizeof (arg));
	if (door_call(d, &arg) < 0)
		printf("door_call FAILED: %s\n", strerror(errno));
	else
		printf("door_call OK\n");

	(void) door_revoke(d);
	return (0);
}
