/* Readiness probe: create and close one ephemeral utun; never change routes/DNS.
 * macOS: cc -Wall -Wextra -o /tmp/treer-utun-probe utun_probe.c
 */
#include <errno.h>
#include <net/if.h>
#include <net/if_utun.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/kern_control.h>
#include <sys/socket.h>
#include <sys/sys_domain.h>
#include <unistd.h>

int main(void) {
    int fd = socket(PF_SYSTEM, SOCK_DGRAM, SYSPROTO_CONTROL);
    if (fd == -1) { perror("socket(PF_SYSTEM)"); return 1; }
    struct ctl_info info = {0};
    snprintf(info.ctl_name, sizeof(info.ctl_name), "%s", UTUN_CONTROL_NAME);
    if (ioctl(fd, CTLIOCGINFO, &info) == -1) {
        perror("CTLIOCGINFO"); close(fd); return 1;
    }
    struct sockaddr_ctl addr = {0};
    addr.sc_len = sizeof(addr);
    addr.sc_family = AF_SYSTEM;
    addr.ss_sysaddr = AF_SYS_CONTROL;
    addr.sc_id = info.ctl_id;
    addr.sc_unit = 0; /* Kernel chooses an unused utun. */
    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) == -1) {
        int error = errno;
        fprintf(stderr, "utun connect: %s (errno=%d, euid=%d)\n", strerror(error), error, geteuid());
        close(fd); return 1;
    }
    char name[IFNAMSIZ] = {0};
    socklen_t size = sizeof(name);
    if (getsockopt(fd, SYSPROTO_CONTROL, UTUN_OPT_IFNAME, name, &size) == -1) {
        perror("UTUN_OPT_IFNAME"); close(fd); return 1;
    }
    printf("created %s, euid=%d; closing immediately, no route or DNS changes\n", name, geteuid());
    close(fd);
    return 0;
}
