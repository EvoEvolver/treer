#include "../../Platform/Bridge.h"
#include <mach/mach.h>

static inline int treer_test_audit_token(audit_token_t *token) {
    mach_msg_type_number_t count = TASK_AUDIT_TOKEN_COUNT;
    return task_info(mach_task_self(), TASK_AUDIT_TOKEN, (task_info_t)token, &count);
}
