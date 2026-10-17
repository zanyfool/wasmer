use super::*;
use crate::syscalls::*;

/// Change cwd using an open directory descriptor, including its current path
/// after a guest-visible rename. Experimental wasm32 ABI: iphone_desk_wasix32_v1.fchdir.
#[instrument(level = "trace", skip_all, fields(%fd), ret)]
pub fn fchdir<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    fd: WasiFd,
    buffer: WasmPtr<u8, M>,
    capacity: M::Offset,
) -> Result<Errno, WasiError> {
    WasiEnv::do_pending_operations(&mut ctx)?;
    let path = wasi_try_ok!(fchdir_path(ctx.data(), fd));
    let capacity: u64 = capacity.into();
    if buffer.is_null() {
        return Ok(Errno::Fault);
    }
    if path.len() as u64 + 1 > capacity {
        return Ok(Errno::Nametoolong);
    }
    // Validate and fill the caller's preallocated buffer before mutating cwd.
    // This lets libc update its own cwd cache without allocating after success.
    {
        let env = ctx.data();
        let (memory, _) = unsafe { env.get_memory_and_wasi_state(&ctx, 0) };
        let mut bytes = path.as_bytes().to_vec();
        bytes.push(0);
        let length = wasi_try_ok!(to_offset::<M>(bytes.len()));
        let output = match buffer.slice(&memory, length) {
            Ok(output) => output,
            Err(_) => return Ok(Errno::Fault),
        };
        if output.write_slice(&bytes).is_err() {
            return Ok(Errno::Fault);
        }
    }
    ctx.data().state.fs.set_current_dir(&path);
    #[cfg(feature = "journal")]
    if ctx.data().enable_journal {
        JournalEffector::save_chdir(&mut ctx, path).map_err(|err| {
            tracing::error!("failed to save fchdir event - {}", err);
            WasiError::Exit(ExitCode::from(Errno::Fault))
        })?;
    }
    #[cfg(not(feature = "journal"))]
    let _ = path;
    Ok(Errno::Success)
}

pub(crate) fn fchdir_path(env: &WasiEnv, fd: WasiFd) -> Result<String, Errno> {
    let state = &env.state;
    let descriptor = state.fs.get_fd(fd)?;
    let path = {
        let guard = descriptor.inode.read();
        match guard.deref() {
            Kind::Root { .. } => "/".to_string(),
            Kind::Dir { path, .. } => crate::fs::PosixPath::from_path(path).as_str().to_owned(),
            _ => return Err(Errno::Notdir),
        }
    };
    // This runtime represents cwd by a guest path. Do not silently select a
    // replacement directory if an open directory was unlinked and recreated.
    // Unlinked cwd objects are not supported by that existing representation.
    let resolved =
        state
            .fs
            .get_inode_at_path(&state.inodes, crate::VIRTUAL_ROOT_FD, &path, true)?;
    if resolved.ino() != descriptor.inode.ino() {
        return Err(Errno::Stale);
    }
    if path != "/" {
        state
            .fs
            .root_fs
            .read_dir(Path::new(&path))
            .map_err(fs_error_into_wasi_err)?;
    }
    Ok(path)
}
