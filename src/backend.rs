//! Native copy-on-write operations. There is deliberately no byte-copy fallback.
//!
//! Directory traversal uses `openat(O_NOFOLLOW)` throughout. Callers should pass
//! canonical root paths, because even symlinks in the roots themselves are refused.

use std::collections::VecDeque;
use std::ffi::{CString, OsString};
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

pub fn name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "apfs-clonefile"
    }
    #[cfg(target_os = "linux")]
    {
        "linux-reflink"
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "unsupported"
    }
}

fn cstring(value: &std::ffi::OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

fn owned_fd(fd: RawFd) -> io::Result<OwnedFd> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: successful open/openat returns a new, owned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn open_dir_at(parent: RawFd, component: &std::ffi::OsStr) -> io::Result<OwnedFd> {
    let component = cstring(component)?;
    // SAFETY: the C string is terminated and the descriptor is live.
    owned_fd(unsafe {
        libc::openat(
            parent,
            component.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    })
}

fn open_root(path: &Path) -> io::Result<OwnedFd> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut dir = owned_fd(unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    })?;
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::CurDir => (),
            Component::Normal(part) => dir = open_dir_at(dir.as_raw_fd(), part)?,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "root path must be canonical and cannot contain '..'",
                ))
            }
        }
    }
    Ok(dir)
}

fn parts(relative: &Path) -> Result<Vec<OsString>, String> {
    let mut result = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => result.push(part.to_os_string()),
            _ => {
                return Err(format!(
                    "expected a normalized repository-relative path: {relative:?}"
                ))
            }
        }
    }
    if result.is_empty() {
        return Err("a file path cannot be empty".into());
    }
    Ok(result)
}

fn parent_fd(root: &Path, relative: &Path, create: bool) -> Result<(OwnedFd, CString), String> {
    let components = parts(relative)?;
    let mut parent = open_root(root)
        .map_err(|e| format!("cannot open root {root:?} without following symlinks: {e}"))?;
    for component in &components[..components.len() - 1] {
        if create {
            let name = cstring(component).map_err(|e| e.to_string())?;
            let rc = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) };
            if rc < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(format!("cannot create parent of {relative:?}: {error}"));
                }
            }
        }
        parent = open_dir_at(parent.as_raw_fd(), component)
            .map_err(|e| format!("unsafe or inaccessible directory in {relative:?}: {e}"))?;
    }
    Ok((
        parent,
        cstring(components.last().unwrap()).map_err(|e| e.to_string())?,
    ))
}

fn stat_fd(fd: RawFd) -> io::Result<libc::stat> {
    let mut stat = std::mem::MaybeUninit::uninit();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { stat.assume_init() })
}

fn clone_error(error: io::Error) -> String {
    match error.raw_os_error() {
        Some(libc::EXDEV) => format!("source and destination must be on the same filesystem/volume: {error}"),
        Some(libc::ENOSPC) => format!("filesystem has insufficient free space for clone metadata or new data: {error}"),
        Some(libc::ENOTSUP) | Some(libc::ENOTTY) | Some(libc::EINVAL) | Some(libc::ENOSYS) =>
            format!("native copy-on-write cloning is unsupported here ({error}); use APFS on macOS or a reflink-capable Linux filesystem on the same volume; no full-copy fallback was performed"),
        _ => format!("native copy-on-write clone failed: {error}; no full-copy fallback was performed"),
    }
}

#[cfg(target_os = "macos")]
fn clone_fd(source: RawFd, parent: RawFd, leaf: &CString) -> io::Result<()> {
    unsafe extern "C" {
        fn fclonefileat(
            src: libc::c_int,
            dst_dir: libc::c_int,
            dst: *const libc::c_char,
            flags: libc::c_uint,
        ) -> libc::c_int;
    }
    // fclonefileat avoids a source pathname race and is in Apple's clonefile family.
    // CLONE_NOOWNERCOPY keeps the destination owned by the invoking user.
    if unsafe { fclonefileat(source, parent, leaf.as_ptr(), 0x0002) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn clone_fd(source: RawFd, parent: RawFd, leaf: &CString) -> io::Result<()> {
    let target = owned_fd(unsafe {
        libc::openat(
            parent,
            leaf.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    })?;
    // FICLONE is the Linux filesystem ioctl, with an int source fd as its argument.
    let result = unsafe { libc::ioctl(target.as_raw_fd(), 0x40049409 as libc::c_ulong, source) };
    if result != 0 {
        let error = io::Error::last_os_error();
        // We created this leaf with O_EXCL; failed clones must not leave empty files.
        unsafe {
            libc::unlinkat(parent, leaf.as_ptr(), 0);
        }
        Err(error)
    } else {
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn clone_fd(_: RawFd, _: RawFd, _: &CString) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this platform has no supported clone backend",
    ))
}

/// Probe on the destination volume using tiny temporary files, without touching
/// the source checkout. Device identity is checked before trying the operation.
pub fn probe(source: &Path, destination_parent: &Path) -> Result<(), String> {
    let source_dir =
        open_root(source).map_err(|e| format!("cannot open source {source:?}: {e}"))?;
    let destination_dir = open_root(destination_parent)
        .map_err(|e| format!("cannot open destination parent {destination_parent:?}: {e}"))?;
    let source_stat = stat_fd(source_dir.as_raw_fd()).map_err(|e| e.to_string())?;
    let destination_stat = stat_fd(destination_dir.as_raw_fd()).map_err(|e| e.to_string())?;
    if source_stat.st_dev != destination_stat.st_dev {
        return Err("source and destination are on different filesystems/volumes; choose a destination on the source volume".into());
    }
    let temporary = tempfile::Builder::new()
        .prefix(".git-thin-probe-")
        .tempdir_in(destination_parent)
        .map_err(|e| format!("cannot create clone probe: {e}"))?;
    let mut file = File::create(temporary.path().join("source"))
        .map_err(|e| format!("cannot create clone probe source: {e}"))?;
    file.write_all(&[0x5a; 4096])
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("cannot write clone probe: {e}"))?;
    drop(file);
    let file = File::open(temporary.path().join("source"))
        .map_err(|e| format!("cannot reopen clone probe source: {e}"))?;
    let parent = open_root(temporary.path()).map_err(|e| e.to_string())?;
    let leaf = CString::new("clone").unwrap();
    clone_fd(file.as_raw_fd(), parent.as_raw_fd(), &leaf).map_err(clone_error)?;
    Ok(())
}

/// Clone a regular tracked file. No source or destination symlink is followed.
pub fn clone_regular(
    source_root: &Path,
    dest_root: &Path,
    relative: &Path,
    executable: bool,
) -> Result<(), String> {
    let source = open_regular(source_root, relative)?;
    let (dest_parent, dest_name) = parent_fd(dest_root, relative, true)?;
    clone_fd(source.as_raw_fd(), dest_parent.as_raw_fd(), &dest_name)
        .map_err(|e| format!("cannot clone {relative:?}: {}", clone_error(e)))?;
    let destination = owned_fd(unsafe {
        libc::openat(
            dest_parent.as_raw_fd(),
            dest_name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    })
    .map_err(|e| format!("cannot verify cloned file {relative:?}: {e}"))?;
    let mode = if executable { 0o755 } else { 0o644 };
    if unsafe { libc::fchmod(destination.as_raw_fd(), mode) } != 0 {
        return Err(format!(
            "cannot set cloned file mode for {relative:?}: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Open a tracked regular file for hashing, protecting every path component
/// against symlink traversal and checking the type of the opened inode.
pub fn open_regular(root: &Path, relative: &Path) -> Result<File, String> {
    let (parent, leaf) = parent_fd(root, relative, false)?;
    let file = owned_fd(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            leaf.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    })
    .map_err(|e| format!("cannot safely open tracked file {relative:?}: {e}"))?;
    let stat = stat_fd(file.as_raw_fd()).map_err(|e| e.to_string())?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(format!("tracked file {relative:?} is not a regular file"));
    }
    Ok(File::from(file))
}

/// Lexical validation also permits dangling links whose target stays in the tree.
pub fn validate_symlink_target(relative: &Path, target: &Path) -> Result<(), String> {
    let components = parts(relative)?;
    let mut depth = components.len() - 1;
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => (),
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => return Err(format!("tracked symlink {relative:?} escapes the workspace or uses an absolute target: {target:?}")),
        }
    }
    if target.as_os_str().is_empty() {
        return Err(format!("tracked symlink {relative:?} has an empty target"));
    }
    Ok(())
}

fn read_link_at(parent: RawFd, name: &CString) -> io::Result<PathBuf> {
    let mut size = 256;
    loop {
        let mut bytes = vec![0u8; size];
        let count = unsafe {
            libc::readlinkat(
                parent,
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if (count as usize) < size {
            bytes.truncate(count as usize);
            return Ok(PathBuf::from(OsString::from_vec(bytes)));
        }
        size *= 2;
        if size > 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "symlink target exceeds 1 MiB",
            ));
        }
    }
}

// Expand existing link chains using lstat/readlink equivalents, never by opening
// their targets. This catches links which are lexically inside but escape through
// another link (for example `inside-link/../outside`).
fn validate_link_chain(root: &Path, relative: &Path, target: &Path) -> Result<(), String> {
    let mut stack = parts(relative)?;
    stack.pop();
    let mut pending: VecDeque<OsString> = target
        .components()
        .map(|c| c.as_os_str().to_os_string())
        .collect();
    let mut links = 0;
    while let Some(component) = pending.pop_front() {
        if component == "." {
            continue;
        }
        if component == ".." {
            if stack.pop().is_none() {
                return Err(format!(
                    "tracked symlink {relative:?} escapes the workspace through a symlink chain"
                ));
            }
            continue;
        }
        if component == "/" {
            return Err(format!(
                "tracked symlink {relative:?} resolves through an absolute symlink"
            ));
        }
        let mut parent = open_root(root).map_err(|e| e.to_string())?;
        let mut missing = false;
        for part in &stack {
            match open_dir_at(parent.as_raw_fd(), part) {
                Ok(dir) => parent = dir,
                Err(e) if matches!(e.raw_os_error(), Some(libc::ENOENT) | Some(libc::ENOTDIR)) => {
                    missing = true;
                    break;
                }
                Err(e) => {
                    return Err(format!(
                        "cannot safely inspect symlink target for {relative:?}: {e}"
                    ))
                }
            }
        }
        if missing {
            stack.push(component);
            continue;
        }
        let name = cstring(&component).map_err(|e| e.to_string())?;
        let mut stat = std::mem::MaybeUninit::uninit();
        let rc = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                stack.push(component);
                continue;
            }
            return Err(format!(
                "cannot inspect symlink target for {relative:?}: {error}"
            ));
        }
        if unsafe { stat.assume_init() }.st_mode & libc::S_IFMT == libc::S_IFLNK {
            links += 1;
            if links > 40 {
                return Err(format!(
                    "tracked symlink {relative:?} has a cycle or a chain exceeding 40 links"
                ));
            }
            let next = read_link_at(parent.as_raw_fd(), &name).map_err(|e| e.to_string())?;
            for part in next.components().rev() {
                pending.push_front(part.as_os_str().to_os_string());
            }
        } else {
            stack.push(component);
        }
    }
    Ok(())
}

pub fn reproduce_symlink(
    source_root: &Path,
    dest_root: &Path,
    relative: &Path,
) -> Result<(), String> {
    let target = read_symlink(source_root, relative)?;
    let (dest_parent, dest_name) = parent_fd(dest_root, relative, true)?;
    let target = cstring(target.as_os_str()).map_err(|e| e.to_string())?;
    if unsafe { libc::symlinkat(target.as_ptr(), dest_parent.as_raw_fd(), dest_name.as_ptr()) } != 0
    {
        return Err(format!(
            "cannot reproduce tracked symlink {relative:?}: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Read and validate a tracked symlink without following it or any parent link.
pub fn read_symlink(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    let (parent, leaf) = parent_fd(root, relative, false)?;
    let target = read_link_at(parent.as_raw_fd(), &leaf)
        .map_err(|e| format!("cannot read tracked symlink {relative:?}: {e}"))?;
    validate_symlink_target(relative, &target)?;
    validate_link_chain(root, relative, &target)?;
    Ok(target)
}

/// Atomically publish a reserved directory without replacing or nesting inside
/// an existing destination. Both parents are opened without symlink traversal.
pub fn rename_directory_noreplace(source: &Path, destination: &Path) -> Result<(), String> {
    let source_parent_path = source.parent().ok_or("source directory has no parent")?;
    let destination_parent_path = destination
        .parent()
        .ok_or("destination directory has no parent")?;
    let source_leaf = source.file_name().ok_or("source must name a directory")?;
    let destination_leaf = destination
        .file_name()
        .ok_or("destination must name a directory")?;
    let source_parent = open_root(source_parent_path)
        .map_err(|e| format!("cannot safely open source parent: {e}"))?;
    let destination_parent = open_root(destination_parent_path)
        .map_err(|e| format!("cannot safely open destination parent: {e}"))?;
    let source_leaf = cstring(source_leaf).map_err(|e| e.to_string())?;
    let destination_leaf = cstring(destination_leaf).map_err(|e| e.to_string())?;
    let mut stat = std::mem::MaybeUninit::uninit();
    if unsafe {
        libc::fstatat(
            source_parent.as_raw_fd(),
            source_leaf.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(format!(
            "cannot inspect directory before publication: {}",
            io::Error::last_os_error()
        ));
    }
    if unsafe { stat.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err("publication source is not a real directory".into());
    }
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            source_parent.as_raw_fd(),
            source_leaf.as_ptr(),
            destination_parent.as_raw_fd(),
            destination_leaf.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = {
        unsafe extern "C" {
            fn renameatx_np(
                from_fd: libc::c_int,
                from: *const libc::c_char,
                to_fd: libc::c_int,
                to: *const libc::c_char,
                flags: libc::c_uint,
            ) -> libc::c_int;
        }
        const RENAME_EXCL: libc::c_uint = 0x00000004;
        unsafe {
            renameatx_np(
                source_parent.as_raw_fd(),
                source_leaf.as_ptr(),
                destination_parent.as_raw_fd(),
                destination_leaf.as_ptr(),
                RENAME_EXCL,
            )
        }
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err("atomic no-replace directory publication is unsupported on this platform".into());
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if result != 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::AlreadyExists {
            return Err("destination appeared during creation; it was left untouched".into());
        }
        return Err(format!("atomic no-replace publication failed: {error}; neither copying nor overwrite fallback was attempted"));
    }
    File::from(source_parent)
        .sync_all()
        .and_then(|_| File::from(destination_parent).sync_all())
        .map_err(|e| format!("directory was published, but syncing its parent failed: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn symlink_scope_is_lexical_and_relative() {
        assert!(validate_symlink_target(Path::new("dir/link"), Path::new("../file")).is_ok());
        assert!(validate_symlink_target(Path::new("link"), Path::new("missing/file")).is_ok());
        assert!(validate_symlink_target(Path::new("link"), Path::new("../outside")).is_err());
        assert!(validate_symlink_target(Path::new("link"), Path::new("/absolute")).is_err());
    }

    #[test]
    fn rejects_escaping_symlink_chains_and_parent_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("source")).unwrap();
        std::fs::create_dir(root.join("dest")).unwrap();
        let source = root.join("source");
        symlink(".", source.join("inside")).unwrap();
        symlink("inside/../outside", source.join("link")).unwrap();
        assert!(reproduce_symlink(&source, &root.join("dest"), Path::new("link")).is_err());
        symlink(".", source.join("parent")).unwrap();
        assert!(
            clone_regular(&source, &root.join("dest"), Path::new("parent/file"), false).is_err()
        );
    }

    #[test]
    fn reproduces_dangling_internal_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("source")).unwrap();
        std::fs::create_dir(root.join("dest")).unwrap();
        symlink("missing", root.join("source/link")).unwrap();
        reproduce_symlink(&root.join("source"), &root.join("dest"), Path::new("link")).unwrap();
        assert_eq!(
            std::fs::read_link(root.join("dest/link")).unwrap(),
            Path::new("missing")
        );
    }

    #[test]
    fn publication_never_replaces_or_nests_in_existing_destination() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let source = root.join("stage");
        let destination = root.join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("tracked"), b"owned").unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("existing"), b"preserve").unwrap();
        assert!(rename_directory_noreplace(&source, &destination).is_err());
        assert_eq!(
            std::fs::read(destination.join("existing")).unwrap(),
            b"preserve"
        );
        assert!(!destination.join("stage").exists());
        assert!(source.join("tracked").exists());
        let fresh = root.join("fresh");
        rename_directory_noreplace(&source, &fresh).unwrap();
        assert!(!source.exists());
        assert_eq!(std::fs::read(fresh.join("tracked")).unwrap(), b"owned");
    }
}
