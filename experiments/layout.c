/* Emit the exact ABI layout doors-sys must reproduce. */
#include <stdio.h>
#include <stddef.h>
#include <sys/types.h>
#include <sys/door.h>
#include <door.h>

#define SZ(t)      printf("size_of(%-18s) = %2zu   align_of = %zu\n", #t, sizeof(t), _Alignof(t))
#define OFF(t, f)  printf("  offset_of(%-14s, %-24s) = %2zu\n", #t, #f, offsetof(t, f))

int main(void) {
	printf("=== scalar types ===\n");
	SZ(door_attr_t); SZ(door_id_t); SZ(door_ptr_t);
	SZ(pid_t); SZ(uid_t); SZ(gid_t); SZ(size_t); SZ(uint_t); SZ(int);

	printf("\n=== door_desc_t ===\n");
	SZ(door_desc_t);
	OFF(door_desc_t, d_attributes);
	OFF(door_desc_t, d_data);
	OFF(door_desc_t, d_data.d_desc.d_descriptor);
	OFF(door_desc_t, d_data.d_desc.d_id);
	OFF(door_desc_t, d_data.d_resv);
	printf("  size_of(d_data union)      = %2zu\n", sizeof(((door_desc_t *)0)->d_data));
	printf("  size_of(d_desc struct)     = %2zu\n", sizeof(((door_desc_t *)0)->d_data.d_desc));

	printf("\n=== door_info_t ===\n");
	SZ(door_info_t);
	OFF(door_info_t, di_target);
	OFF(door_info_t, di_proc);
	OFF(door_info_t, di_data);
	OFF(door_info_t, di_attributes);
	OFF(door_info_t, di_uniquifier);
	OFF(door_info_t, di_resv);

	printf("\n=== door_arg_t ===\n");
	SZ(door_arg_t);
	OFF(door_arg_t, data_ptr);
	OFF(door_arg_t, data_size);
	OFF(door_arg_t, desc_ptr);
	OFF(door_arg_t, desc_num);
	OFF(door_arg_t, rbuf);
	OFF(door_arg_t, rsize);

	printf("\n=== door_cred_t ===\n");
	SZ(door_cred_t);
	OFF(door_cred_t, dc_euid);
	OFF(door_cred_t, dc_egid);
	OFF(door_cred_t, dc_ruid);
	OFF(door_cred_t, dc_rgid);
	OFF(door_cred_t, dc_pid);
	OFF(door_cred_t, dc_resv);

	printf("\n=== constants ===\n");
	printf("DOOR_UNREF            = 0x%x\n", DOOR_UNREF);
	printf("DOOR_PRIVATE          = 0x%x\n", DOOR_PRIVATE);
	printf("DOOR_UNREF_MULTI      = 0x%x\n", DOOR_UNREF_MULTI);
	printf("DOOR_REFUSE_DESC      = 0x%x\n", DOOR_REFUSE_DESC);
	printf("DOOR_NO_CANCEL        = 0x%x\n", DOOR_NO_CANCEL);
	printf("DOOR_NO_DEPLETION_CB  = 0x%x\n", DOOR_NO_DEPLETION_CB);
	printf("DOOR_PRIVCREATE       = 0x%x\n", DOOR_PRIVCREATE);
	printf("DOOR_LOCAL            = 0x%x\n", DOOR_LOCAL);
	printf("DOOR_REVOKED          = 0x%x\n", DOOR_REVOKED);
	printf("DOOR_IS_UNREF         = 0x%x\n", DOOR_IS_UNREF);
	printf("DOOR_DEPLETION_CB     = 0x%x\n", DOOR_DEPLETION_CB);
	printf("DOOR_DESCRIPTOR       = 0x%x\n", DOOR_DESCRIPTOR);
	printf("DOOR_RELEASE          = 0x%x\n", DOOR_RELEASE);
	printf("DOOR_INVAL            = %d\n",   DOOR_INVAL);
	printf("DOOR_QUERY            = %d\n",   DOOR_QUERY);
	printf("DOOR_UNREF_DATA       = %p\n",   DOOR_UNREF_DATA);
	printf("DOOR_PARAM_DESC_MAX   = %d\n",   DOOR_PARAM_DESC_MAX);
	printf("DOOR_PARAM_DATA_MAX   = %d\n",   DOOR_PARAM_DATA_MAX);
	printf("DOOR_PARAM_DATA_MIN   = %d\n",   DOOR_PARAM_DATA_MIN);
	printf("DOOR_CREATE_MASK      = 0x%x\n", DOOR_CREATE_MASK);
	printf("DOOR_ATTR_MASK        = 0x%x\n", DOOR_ATTR_MASK);
	return 0;
}
