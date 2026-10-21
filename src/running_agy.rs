use anyhow::Result;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Process {
    pub pid: u32,
    pub parent: String,
    pub path: String,
    pub name: String,
}

fn select_agy(processes: Vec<Process>) -> Vec<Process> {
    processes.into_iter().filter(|p| p.name.eq_ignore_ascii_case("agy.exe")).collect()
}

#[cfg(not(windows))]
pub(crate) fn list() -> Result<Vec<Process>> { Ok(select_agy(Vec::new())) }

#[cfg(windows)]
pub(crate) fn list() -> Result<Vec<Process>> {
    use std::{collections::HashMap, mem::size_of};
    use windows_sys::Win32::{Foundation::{CloseHandle, INVALID_HANDLE_VALUE}, System::{
        Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS},
        Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW},
    }};
    struct Handle(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for Handle { fn drop(&mut self) { unsafe { CloseHandle(self.0); } } }
    fn string(wide: &[u16]) -> String { String::from_utf16_lossy(&wide[..wide.iter().position(|v| *v == 0).unwrap_or(wide.len())]) }
    // Snapshot is owned until all entries have been copied.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE { return Err(std::io::Error::last_os_error().into()); }
    let snapshot = Handle(snapshot);
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut entries = Vec::new();
    let mut ok = unsafe { Process32FirstW(snapshot.0, &mut entry) };
    while ok != 0 {
        entries.push((entry.th32ProcessID, entry.th32ParentProcessID, string(&entry.szExeFile)));
        ok = unsafe { Process32NextW(snapshot.0, &mut entry) };
    }
    let names: HashMap<_, _> = entries.iter().map(|(pid, _, name)| (*pid, name.clone())).collect();
    let mut processes = Vec::new();
    for (pid, parent, name) in entries {
        if !name.eq_ignore_ascii_case("agy.exe") { continue; }
        let mut path = name.clone();
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if !handle.is_null() {
            let handle = Handle(handle);
            let mut buffer = vec![0u16; 32768];
            let mut len = buffer.len() as u32;
            if unsafe { QueryFullProcessImageNameW(handle.0, 0, buffer.as_mut_ptr(), &mut len) } != 0 {
                path = String::from_utf16_lossy(&buffer[..len as usize]);
            }
        }
        processes.push(Process { pid, parent: names.get(&parent).cloned().unwrap_or_else(|| "unknown".into()), path, name });
    }
    Ok(select_agy(processes))
}

pub(crate) fn format_processes(processes: &[Process]) -> String {
    processes.iter().map(|p| format!("PID {} (parent: {}, path: {})", p.pid, p.parent, p.path)).collect::<Vec<_>>().join("\n")
}

pub(crate) fn warn(processes: &[Process]) {
    if !processes.is_empty() {
        crate::output::user_println(&format!("Warning: running agy processes keep the previous account and may write it back when their token refreshes. Restart them to use the new account.\n{}", format_processes(processes)));
    }
}

pub(crate) fn restart(processes: &[Process]) -> Result<Vec<u32>> {
    let mut stopped = Vec::new();
    for process in select_agy(processes.to_vec()) {
        #[cfg(windows)]
        {
            // Re-snapshot before each signal: never target an unrelated name from a stale list.
            if !list()?.iter().any(|p| p.pid == process.pid && p.path == process.path) { continue; }
            let pid = process.pid.to_string();
            let status = std::process::Command::new("taskkill.exe").args(["/PID", &pid, "/T"]).output()?;
            if !status.status.success() && !list()?.iter().any(|p| p.pid == process.pid) { continue; }
            std::thread::sleep(std::time::Duration::from_secs(3));
            if list()?.iter().any(|p| p.pid == process.pid && p.path == process.path) {
                let status = std::process::Command::new("taskkill.exe").args(["/PID", &pid, "/T", "/F"]).output()?;
                if !status.status.success() { anyhow::bail!("could not stop agy PID {}", process.pid); }
            }
            stopped.push(process.pid);
            crate::output::user_println(&format!("Stopped agy PID {} ({}). Start agy again to use the new account.", process.pid, process.parent));
        }
        #[cfg(not(windows))]
        { let _ = process; }
    }
    Ok(stopped)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats_running_agy_and_selects_only_agy_for_restart() {
        let fake = |pid, name: &str| Process { pid, name: name.into(), parent: "Code.exe".into(), path: format!("C:\\bin\\{name}") };
        let selected = select_agy(vec![fake(12, "agy.exe"), fake(13, "Code.exe"), fake(14, "AGY.EXE"), fake(15, "agy.cmd")]);
        assert_eq!(selected.iter().map(|p| p.pid).collect::<Vec<_>>(), [12, 14]);
        assert_eq!(format_processes(&selected[..1]), "PID 12 (parent: Code.exe, path: C:\\bin\\agy.exe)");
    }
}
