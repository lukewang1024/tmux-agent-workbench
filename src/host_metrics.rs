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

/// Match the standalone macOS widget: active, wired and compressed pages.
#[cfg(target_os = "macos")]
#[allow(deprecated)] // Mach is exposed by the existing libc dependency.
pub(crate) fn used_memory_bytes() -> io::Result<u64> {
    // SAFETY: the Mach API fills a correctly sized VM statistics structure.
    let mut stats = unsafe { std::mem::zeroed::<libc::vm_statistics64>() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    let result = unsafe {
        let host = libc::mach_host_self();
        let result = libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            (&mut stats as *mut libc::vm_statistics64).cast(),
            &mut count,
        );
        mach_port_deallocate(libc::mach_task_self(), host);
        result
    };
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    // Older supported macOS releases return fewer trailing fields than the
    // SDK's current structure. The compressor counter is the last field we
    // read, so accepting the older prefix preserves the standalone metric.
    let required_bytes = std::mem::offset_of!(libc::vm_statistics64, compressor_page_count)
        + std::mem::size_of::<libc::natural_t>();
    let required_count = required_bytes.div_ceil(std::mem::size_of::<libc::integer_t>()) as u32;
    if result != libc::KERN_SUCCESS || count < required_count || page_size <= 0 {
        return Err(io::Error::other(format!(
            "memory statistics unavailable: result={result} count={count} required={required_count} page_size={page_size}"
        )));
    }
    Ok((u64::from(stats.active_count)
        + u64::from(stats.wire_count)
        + u64::from(stats.compressor_page_count))
    .saturating_mul(page_size as u64))
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

    #[test]
    fn native_memory_pages_are_readable_without_privileges() {
        assert!(super::used_memory_bytes().unwrap() > 0);
    }
}
