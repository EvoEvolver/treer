//! Darwin's killpg1 excludes zombies when counting eligible group members:
//! https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_sig.c
//! This exception requires the exited, unreaped owned child to be the ONLY
//! group member. An EPERM with any other member is still an error (even if that
//! member might also be a zombie); do not infer success from permission denial.

pub(super) fn only_owned_leader(group: rustix::process::Pid) -> bool {
    // Two slots suffice: any second member or full/truncated buffer rejects
    // the singleton exception. proc_listpgrppids returns a PID COUNT and scans
    // both allproc and zombproc under the kernel's process-list lock:
    // xnu/bsd/kern/proc_info.c (proc_listpids) and
    // xnu/libsyscall/wrappers/libproc/libproc.c (proc_listpgrppids).
    let mut pids = [0_i32; 2];
    // SAFETY: initialized owned i32 storage covers the complete byte capacity.
    // Read a returned PID only after checking an exact one-element result.
    // No paths, argv or environment are read. The caller retains the leader
    // unreaped, and has independently proved its EXITED state with WNOWAIT.
    let count = unsafe {
        libc::proc_listpgrppids(
            group.as_raw_nonzero().get(),
            pids.as_mut_ptr().cast(),
            size_of_val(&pids) as i32,
        )
    };
    count == 1 && pids[0] == group.as_raw_nonzero().get()
}
