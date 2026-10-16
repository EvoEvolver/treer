//! The only new FFI is fixed-size libproc output into initialized owned storage.
use std::{ffi::OsStr, io, mem::size_of, os::unix::ffi::OsStrExt, path::PathBuf};

use super::{invalid, Identity};

pub(super) fn creation(pid: u32) -> io::Result<Identity> {
    // SAFETY: proc_bsdinfo is a C integer/char-array POD; all-zero is valid. The
    // initialized output object has exactly the size passed to proc_pidinfo.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    // SAFETY: writable buffer is live and valid for the complete fixed size.
    // Reject short/failed writes before using any kernel-returned fields.
    let length = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size_of::<libc::proc_bsdinfo>() as i32,
        )
    };
    if length != size_of::<libc::proc_bsdinfo>() as i32
        || info.pbi_pid != pid
        || info.pbi_uid != rustix::process::geteuid().as_raw()
        || info.pbi_start_tvsec == 0
        || info.pbi_start_tvusec >= 1_000_000
    {
        return Err(invalid());
    }
    Ok(Identity {
        creation: format!("macos:{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
        parent: info.pbi_ppid,
    })
}

pub(super) fn children(pid: u32) -> io::Result<Vec<u32>> {
    // One extra slot detects a truncated list; never silently accept an
    // inventory exceeding 4096 children. Zero-initialize the entire allocation.
    let mut pids = vec![0_i32; 4097];
    // SAFETY: libproc writes at most the byte size of this initialized allocation;
    // only the returned complete i32 elements are read below. This wrapper
    // returns a PID COUNT (unlike proc_listpids, which returns bytes):
    // apple-oss-distributions/xnu/libsyscall/wrappers/libproc/libproc.c.
    let length = unsafe {
        libc::proc_listchildpids(
            pid as i32,
            pids.as_mut_ptr().cast(),
            (pids.len() * size_of::<i32>()) as i32,
        )
    };
    if !(0..=4096).contains(&length) {
        return Err(invalid());
    }
    pids.truncate(length as usize);
    pids.into_iter()
        .map(|pid| {
            u32::try_from(pid)
                .ok()
                .filter(|pid| *pid != 0)
                .ok_or_else(invalid)
        })
        .collect()
}

pub(super) fn image_path(pid: u32) -> io::Result<PathBuf> {
    let mut buffer = [0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the initialized array has the complete advertised capacity; the
    // kernel cannot extend a Rust reference beyond the allocated object. Scan
    // only this owned buffer and reject missing/interior unexpected terminators.
    let length =
        unsafe { libc::proc_pidpath(pid as i32, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if length <= 0 || length as usize >= buffer.len() {
        return Err(invalid());
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(invalid)?;
    if end != length as usize {
        return Err(invalid());
    }
    let path = PathBuf::from(OsStr::from_bytes(&buffer[..end]));
    if !path.is_absolute() {
        return Err(invalid());
    }
    Ok(path)
}

// Layout from the SDK's sys/proc_info.h: proc_regioninfo followed by
// vnode_info_path. Keep this platform ABI local; exact returned size is required.
#[repr(C)]
struct RegionInfo {
    protection: u32,
    max_protection: u32,
    inheritance: u32,
    flags: u32,
    offset: u64,
    counters: [u32; 14],
    address: u64,
    size: u64,
}
#[repr(C)]
struct RegionWithPath {
    region: RegionInfo,
    vnode: libc::vnode_info_path,
}

/// proc_pidpath alone is a name, not a live executable inode: after rename it
/// can still name the replacement. Require the first file-backed executable VM
/// region to agree with that name AND compare its kernel-reported vnode below.
/// Unusual address layouts, missing paths, protected processes or truncated ABI
/// output fail closed rather than substituting a disk filename for this proof.
pub(super) fn mapped_image_identity(pid: u32, path: &std::path::Path) -> io::Result<(u32, u64)> {
    let mut address = 0;
    for _ in 0..256 {
        // SAFETY: all fields are integer/char arrays, zero is valid; this exact
        // initialized object is the entire writable output allocation.
        let mut info: RegionWithPath = unsafe { std::mem::zeroed() };
        // SAFETY: buffersize equals the complete owned object's size. Flavor 8
        // is PROC_PIDREGIONPATHINFO; reject any non-exact kernel return length.
        let length = unsafe {
            libc::proc_pidinfo(
                pid as i32,
                8,
                address,
                (&mut info as *mut RegionWithPath).cast(),
                size_of::<RegionWithPath>() as i32,
            )
        };
        if length != size_of::<RegionWithPath>() as i32 {
            return Err(invalid());
        }
        let next = info
            .region
            .address
            .checked_add(info.region.size)
            .ok_or_else(invalid)?;
        if next <= address {
            return Err(invalid());
        }
        address = next;
        let stat = &info.vnode.vip_vi.vi_stat;
        if info.region.protection & 4 == 0 || stat.vst_mode & libc::S_IFMT != libc::S_IFREG {
            continue;
        }
        let bytes: Vec<u8> = info
            .vnode
            .vip_path
            .iter()
            .flatten()
            .map(|c| *c as u8)
            .collect();
        let end = bytes.iter().position(|c| *c == 0).ok_or_else(invalid)?;
        if end == 0
            || std::path::Path::new(OsStr::from_bytes(&bytes[..end])) != path
            || stat.vst_ino == 0
        {
            return Err(invalid());
        }
        return Ok((stat.vst_dev, stat.vst_ino));
    }
    Err(invalid())
}
