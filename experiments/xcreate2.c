/*
 * Does door_xcreate require DOOR_PRIVATE?
 *
 * Run one case per process, so a crash in one does not hide the rest.
 *   ./xcreate2 <case>
 */
#include <door.h>
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

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

	(void) pthread_attr_init(&attr);
	(void) pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
	(void) pthread_attr_setstacksize(&attr, 256 * 1024);
	if (pthread_create(&t, &attr, f, arg) != 0)
		return (-1);
	return (0);
}

int
main(int argc, char **argv)
{
	uint_t attrs;
	int d, which;

	setvbuf(stdout, NULL, _IONBF, 0);
	which = (argc > 1) ? atoi(argv[1]) : 0;

	switch (which) {
	case 0: attrs = DOOR_NO_CANCEL; break;
	case 1: attrs = DOOR_PRIVATE; break;
	case 2: attrs = DOOR_PRIVATE | DOOR_NO_CANCEL; break;
	case 3: attrs = DOOR_PRIVATE | DOOR_NO_CANCEL | DOOR_REFUSE_DESC;
		break;
	case 4: attrs = DOOR_PRIVATE | DOOR_NO_CANCEL | DOOR_UNREF; break;
	case 5: attrs = 0; break;
	default: attrs = DOOR_PRIVATE; break;
	}

	printf("case %d attrs=0x%x ... ", which, attrs);

	errno = 0;
	d = door_xcreate(proc, NULL, attrs, create_thread, NULL, NULL, 1);
	if (d < 0) {
		printf("FAILED errno=%d (%s)\n", errno, strerror(errno));
		return (1);
	}
	printf("OK fd=%d", d);

	/* Prove it works: call it from this process. */
	{
		door_arg_t arg;
		(void) memset(&arg, 0, sizeof (arg));
		if (door_call(d, &arg) < 0)
			printf("  but door_call failed: %s\n", strerror(errno));
		else
			printf("  and door_call worked\n");
	}
	(void) door_revoke(d);
	return (0);
}
