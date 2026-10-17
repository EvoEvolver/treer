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

pub(super) fn quiescent(group: rustix::process::Pid) -> std::io::Result<bool> {
    use std::{io, mem::size_of};
    let invalid = || io::Error::other("child group quiescence unavailable");
    let mut pids = vec![0_i32; 4097];
    // SAFETY: initialized owned storage has exactly the advertised byte
    // capacity. The wrapper returns complete PID elements, not a byte count.
    let count = unsafe {
        libc::proc_listpgrppids(
            group.as_raw_nonzero().get(),
            pids.as_mut_ptr().cast(),
            (pids.len() * size_of::<i32>()) as i32,
        )
    };
    if !(1..=4096).contains(&count) {
        return Err(invalid());
    }
    pids.truncate(count as usize);
    if !pids.contains(&group.as_raw_nonzero().get()) {
        return Err(invalid());
    }
    for pid in pids {
        if pid <= 1 {
            return Err(invalid());
        }
        // SAFETY: proc_bsdinfo is a fixed-size C integer/char-array POD, all
        // zero is valid. Only consume kernel fields after an exact-size write.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        // SAFETY: valid initialized fixed-size writable object. arg=1 asks
        // PROC_PIDTBSDINFO to include zombies; arg=0 returns ESRCH for them.
        // Source: xnu/bsd/kern/proc_info.c proc_pidinfo's findzomb handling.
        let length = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                1,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size_of::<libc::proc_bsdinfo>() as i32,
            )
        };
        if length == 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            // A member can be reaped after the atomic inventory. Re-inventory
            // under the same leader reservation; ESRCH itself proves nothing.
            return Ok(false);
        }
        if length != size_of::<libc::proc_bsdinfo>() as i32
            || info.pbi_pid != pid as u32
            || info.pbi_pgid != group.as_raw_nonzero().get() as u32
        {
            return Err(invalid());
        }
        if info.pbi_status != libc::SZOMB {
            return Ok(false);
        }
    }
    Ok(true)
}
