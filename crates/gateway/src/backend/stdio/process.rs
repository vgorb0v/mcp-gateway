pub(super) fn apply_backend_resource_policy(pid: u32) {
    #[cfg(target_os = "macos")]
    {
        if let Err(err) = try_apply_darwin_background(pid) {
            tracing::debug!(pid, error = %err, "failed to apply Darwin background policy");
            let _ = try_apply_nice(pid, 5);
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = try_apply_nice(pid, 5);
    }
}

#[cfg(target_os = "macos")]
fn try_apply_darwin_background(pid: u32) -> std::io::Result<()> {
    let rc = unsafe {
        libc::setpriority(
            libc::PRIO_DARWIN_PROCESS,
            pid as libc::id_t,
            libc::PRIO_DARWIN_BG,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn try_apply_nice(pid: u32, priority: libc::c_int) -> std::io::Result<()> {
    let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, priority) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
pub(super) fn terminate_process_group(pid: u32, signal: libc::c_int) {
    let pgid = -(pid as libc::pid_t);
    unsafe {
        libc::kill(pgid, signal);
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_background_policy_can_be_applied_to_spawned_child() {
        use std::process::Command;

        let mut child = Command::new("/bin/sleep")
            .arg("1")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();

        let result = super::try_apply_darwin_background(pid);

        let _ = child.kill();
        let _ = child.wait();
        assert!(
            result.is_ok(),
            "Darwin background policy failed: {result:?}"
        );
    }
}
