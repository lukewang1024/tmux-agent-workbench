use std::io;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn mach_port_deallocate(
        task: libc::mach_port_t,
        name: libc::mach_port_t,
    ) -> libc::kern_return_t;
}

/// Read aggregate CPU ticks without inventorying processes or touching files.
#[cfg(target_os = "macos")]
#[allow(deprecated)] // Mach is exposed by the existing libc dependency.
pub fn cpu_counters() -> io::Result<(u64, u64)> {
    let mut load = libc::host_cpu_load_info { cpu_ticks: [0; 4] };
    let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
    // SAFETY: load has the layout and capacity required by HOST_CPU_LOAD_INFO.
    let result = unsafe {
        let host = libc::mach_host_self();
        let result = libc::host_statistics(
            host,
            libc::HOST_CPU_LOAD_INFO,
            (&mut load as *mut libc::host_cpu_load_info).cast(),
            &mut count,
        );
        mach_port_deallocate(libc::mach_task_self(), host);
        result
    };
    if result != libc::KERN_SUCCESS || count != libc::HOST_CPU_LOAD_INFO_COUNT {
        return Err(io::Error::other(format!(
            "CPU statistics unavailable: {result}"
        )));
    }
    Ok((
        load.cpu_ticks.iter().map(|&tick| u64::from(tick)).sum(),
        u64::from(load.cpu_ticks[libc::CPU_STATE_IDLE as usize]),
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn cpu_counters() -> io::Result<(u64, u64)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native CPU counters require macOS",
    ))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn native_cpu_ticks_are_readable_without_privileges() {
        let (total, idle) = super::cpu_counters().unwrap();
        assert!(total > 0);
        assert!(idle <= total);
    }
}
