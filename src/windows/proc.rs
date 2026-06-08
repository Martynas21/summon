use std::ffi::c_void;
use windows_sys::Win32::Foundation::FALSE;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
};

pub fn argv_for_pid(pid: u32) -> Option<Vec<String>> {
    unsafe { read_cmdline(pid) }
}

pub fn cmdline_contains(pid: u32, needle: &str) -> bool {
    argv_for_pid(pid)
        .map(|args| args.iter().any(|a| a.contains(needle)))
        .unwrap_or(false)
}

// ── NT internals (stable across Windows versions) ────────────────────────────

#[repr(C)]
struct ProcessBasicInformation {
    reserved1: isize,
    peb_base_address: usize,
    reserved2: [isize; 4],
}

// Offset of CommandLine UNICODE_STRING within RTL_USER_PROCESS_PARAMETERS (x64)
const CMDLINE_OFFSET: usize = 0x70;

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationProcess(
        process_handle: *mut c_void,
        process_information_class: u32,
        process_information: *mut c_void,
        process_information_length: u32,
        return_length: *mut u32,
    ) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn ReadProcessMemory(
        hprocess: *mut c_void,
        lpbaseaddress: *const c_void,
        lpbuffer: *mut c_void,
        nsize: usize,
        lpnumberofbytesread: *mut usize,
    ) -> i32;
}

unsafe fn read_cmdline(pid: u32) -> Option<Vec<String>> {
    let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, FALSE, pid);
    if handle.is_null() {
        return None;
    }
    let _guard = HandleGuard(handle);

    // Get PEB base address
    let mut pbi = ProcessBasicInformation {
        reserved1: 0,
        peb_base_address: 0,
        reserved2: [0; 4],
    };
    let status = NtQueryInformationProcess(
        handle,
        0,
        &mut pbi as *mut _ as *mut c_void,
        std::mem::size_of::<ProcessBasicInformation>() as u32,
        std::ptr::null_mut(),
    );
    if status < 0 {
        return None;
    }

    // PEB.ProcessParameters is at offset 0x20 on x64
    let process_params_ptr_addr = pbi.peb_base_address + 0x20;
    let mut process_params_ptr: u64 = 0;
    let mut bytes_read = 0usize;
    let ok = ReadProcessMemory(
        handle,
        process_params_ptr_addr as *const c_void,
        &mut process_params_ptr as *mut u64 as *mut c_void,
        8,
        &mut bytes_read,
    );
    if ok == 0 || bytes_read < 8 {
        return None;
    }

    // Read CommandLine UNICODE_STRING from RTL_USER_PROCESS_PARAMETERS
    let cmdline_addr = process_params_ptr as usize + CMDLINE_OFFSET;
    // UNICODE_STRING: Length(u16) + MaxLength(u16) + _pad(u32) + Buffer(*u16 as u64)
    let mut us_len: u16 = 0;
    let mut us_buf: u64 = 0;
    let mut dummy = 0usize;

    ReadProcessMemory(
        handle,
        cmdline_addr as *const c_void,
        &mut us_len as *mut u16 as *mut c_void,
        2,
        &mut dummy,
    );
    ReadProcessMemory(
        handle,
        (cmdline_addr + 8) as *const c_void,
        &mut us_buf as *mut u64 as *mut c_void,
        8,
        &mut dummy,
    );

    if us_len == 0 || us_buf == 0 {
        return None;
    }

    let char_count = (us_len / 2) as usize;
    let mut buf: Vec<u16> = vec![0u16; char_count];
    let ok = ReadProcessMemory(
        handle,
        us_buf as *const c_void,
        buf.as_mut_ptr() as *mut c_void,
        us_len as usize,
        &mut bytes_read,
    );
    if ok == 0 {
        return None;
    }

    let cmdline = String::from_utf16_lossy(&buf);
    Some(split_cmdline(&cmdline))
}

fn split_cmdline(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    for c in s.chars() {
        match c {
            '"' => in_quote = !in_quote,
            ' ' | '\t' if !in_quote => {
                if !current.is_empty() {
                    args.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

struct HandleGuard(*mut c_void);
impl Drop for HandleGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe { CloseHandle(self.0) };
    }
}
