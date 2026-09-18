//! Per-PID argv lookup via `sysctl(KERN_PROCARGS2)`.
//!
//! Same mechanism used by `ps`/`procinfo`. Cheap (one syscall, ~1ms even for
//! processes with large environments), and works without elevated privileges
//! for processes owned by the same uid.

use libc::{c_int, c_void, sysctl, CTL_KERN};

// Not exposed by the `libc` crate on every release; value is stable in xnu.
const KERN_ARGMAX: c_int = 8;
const KERN_PROCARGS2: c_int = 49;

/// Returns the argv (argv[0]..argv[argc-1]) of `pid`, or `None` if the kernel
/// rejects the read (process gone, EPERM across uid boundaries, etc.).
///
/// The kernel layout returned by `KERN_PROCARGS2`:
///   `[i32 argc][exec_path\0][padding\0...][argv[0]\0]...[argv[argc-1]\0][env...]`
/// We deliberately stop after argv and ignore env strings — environments can
/// be megabytes (Chrome) and the caller only ever wants a substring match
/// against command-line flags.
pub fn argv_for_pid(pid: i32) -> Option<Vec<String>> {
    let argmax = kern_argmax()?;
    let mut buf: Vec<u8> = vec![0u8; argmax];
    let mut size = argmax;
    let mut mib: [c_int; 3] = [CTL_KERN, KERN_PROCARGS2, pid];
    let rc = unsafe {
        sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buf.as_mut_ptr() as *mut c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc < 0 {
        return None;
    }
    buf.truncate(size);
    parse_procargs2(&buf)
}

fn kern_argmax() -> Option<usize> {
    let mut argmax: c_int = 0;
    let mut size: libc::size_t = std::mem::size_of::<c_int>();
    let mut mib: [c_int; 2] = [CTL_KERN, KERN_ARGMAX];
    let rc = unsafe {
        sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            &mut argmax as *mut _ as *mut c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc < 0 || argmax <= 0 {
        None
    } else {
        Some(argmax as usize)
    }
}

fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
    if buf.len() < 4 {
        return None;
    }
    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if argc < 0 {
        return None;
    }
    let argc = argc as usize;
    let mut pos = 4usize;
    // Skip exec_path (null-terminated) then any zero-padding before argv[0].
    while pos < buf.len() && buf[pos] != 0 {
        pos += 1;
    }
    while pos < buf.len() && buf[pos] == 0 {
        pos += 1;
    }
    let mut argv = Vec::with_capacity(argc);
    for _ in 0..argc {
        if pos >= buf.len() {
            break;
        }
        let start = pos;
        while pos < buf.len() && buf[pos] != 0 {
            pos += 1;
        }
        let s = String::from_utf8_lossy(&buf[start..pos]).into_owned();
        argv.push(s);
        if pos < buf.len() {
            pos += 1;
        }
    }
    Some(argv)
}

/// `true` if any argv element of `pid` contains `needle`. Convenience wrapper
/// — returns `false` when argv lookup fails (treat unreadable processes as
/// non-matches rather than aborting the whole enumeration).
pub fn cmdline_contains(pid: i32, needle: &str) -> bool {
    argv_for_pid(pid)
        .map(|argv| argv.iter().any(|a| a.contains(needle)))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_trivial_argv() {
        // argc=2, exec_path="/x", padding, argv=["a", "bb"]
        let mut buf = Vec::new();
        buf.extend_from_slice(&2i32.to_ne_bytes());
        buf.extend_from_slice(b"/x\0\0\0\0");
        buf.extend_from_slice(b"a\0");
        buf.extend_from_slice(b"bb\0");
        let argv = parse_procargs2(&buf).unwrap();
        assert_eq!(argv, vec!["a".to_string(), "bb".to_string()]);
    }

    #[test]
    fn argv_for_self_includes_known_arg() {
        // The test binary's argv[0] is something cargo-y; not portable to
        // assert. But the call must succeed.
        let pid = std::process::id() as i32;
        let argv = argv_for_pid(pid).expect("self argv");
        assert!(!argv.is_empty());
    }
}
