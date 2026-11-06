//! Sedikit pembungkus API Windows.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const DETACHED_PROCESS: u32 = 0x0000_0008;
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
pub const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Path lengkap alat bawaan Windows (System32), supaya tidak bisa "dibajak" oleh exe
/// bernama sama di folder herdlite, folder kerja, atau PATH.
pub fn sys(name: &str) -> std::path::PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let sys32 = std::path::PathBuf::from(root).join("System32");
    if name.eq_ignore_ascii_case("powershell.exe") {
        sys32.join("WindowsPowerShell").join("v1.0").join("powershell.exe")
    } else {
        sys32.join(name)
    }
}

/// Apakah proses dengan PID ini masih hidup?
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return false;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        ok != 0 && code == STILL_ACTIVE as u32
    }
}

/// Nama file exe dari proses dengan PID ini (mis. "herdlited.exe").
pub fn pid_image(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return None;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit('\\').next().map(|s| s.to_string())
    }
}

/// Apakah PID ini masih hidup DAN memang menjalankan exe dengan nama itu?
/// Mencegah PID lama di file pid (yang sudah dipakai ulang proses lain) ikut dimatikan.
pub fn pid_is(pid: u32, exe: &str) -> bool {
    pid_alive(pid) && pid_image(pid).map(|n| n.eq_ignore_ascii_case(exe)).unwrap_or(false)
}

/// Matikan proses beserta anak-anaknya.
pub fn kill_tree(pid: u32) {
    let _ = Command::new(sys("taskkill.exe"))
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

/// Masukkan proses ini ke Job Object "kill on close": kalau herdlited mati
/// (termasuk crash), semua php-cgi yang dijalankannya ikut mati.
pub fn kill_children_on_exit() {
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        AssignProcessToJobObject(job, GetCurrentProcess());
        // handle sengaja tidak ditutup: job hidup selama proses ini hidup.
    }
}

/// Jadikan stdin/stdout/stderr proses ini tidak diwariskan ke proses anak.
/// Tanpa ini, server latar belakang ikut memegang pipe output terminal
/// (mis. `hl start | more`) sehingga pembaca pipe menunggu selamanya.
pub fn no_inherit_std_handles() {
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    unsafe {
        for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let h = GetStdHandle(id);
            if !h.is_null() && h != INVALID_HANDLE_VALUE {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Cari nama proses yang sedang LISTEN di port TCP tertentu (untuk pesan error).
pub fn port_owner(port: u16) -> Option<String> {
    let out = Command::new(sys("netstat.exe"))
        .args(["-ano", "-p", "TCP"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let suffix = format!(":{port}");
    let pid = text.lines().find_map(|l| {
        let cols: Vec<&str> = l.split_whitespace().collect();
        if cols.len() >= 5 && cols[1].ends_with(&suffix) && cols[3].eq_ignore_ascii_case("LISTENING") {
            cols[4].parse::<u32>().ok()
        } else {
            None
        }
    })?;
    let out = Command::new(sys("tasklist.exe"))
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let t = String::from_utf8_lossy(&out.stdout);
    let name = t.split(',').next().unwrap_or("").trim_matches(|c| c == '"' || c == '\r' || c == '\n');
    Some(if name.is_empty() { format!("PID {pid}") } else { format!("{name} (PID {pid})") })
}
