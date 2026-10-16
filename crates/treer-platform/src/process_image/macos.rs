//! The only new FFI is fixed-size libproc output into initialized owned storage.
use std::{ffi::OsStr, io, mem::size_of, os::unix::ffi::OsStrExt, path::PathBuf};

use super::invalid;

pub(super) fn creation(pid: u32) -> io::Result<String> {
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
    Ok(format!(
        "macos:{}:{}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
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
