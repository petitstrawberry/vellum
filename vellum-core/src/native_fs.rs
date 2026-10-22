//! Exclusive staging creation, adapted from Scarlet desktop_utilities/native_fs.rs.
//!
//! Requires Scarlet Rust 26d00a8 or newer for ordinary std filesystem operations.
//! The remaining native gap is std's probe-then-create implementation and lack
//! of chmod. The checked VFS ABI creates the file exclusively with its initial
//! owner R/W/X capabilities; POSIX group bits, ACLs and ownership are unavailable.
use std::{fs::File, io, path::Path};

#[cfg(not(target_os = "scarlet"))]
pub(crate) fn create_staging(path: &Path, original: Option<&Path>) -> io::Result<File> {
    let permissions = original
        .map(std::fs::metadata)
        .transpose()?
        .map(|m| m.permissions());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    if let Some(permissions) = permissions {
        if let Err(error) = file.set_permissions(permissions) {
            drop(file);
            let _ = std::fs::remove_file(path);
            return Err(error);
        }
    }
    Ok(file)
}

#[cfg(target_os = "scarlet")]
pub(crate) fn create_staging(path: &Path, original: Option<&Path>) -> io::Result<File> {
    use scarlet_abi::fs::*;
    use scarlet_sys::{RawFileMetadata, Syscall, syscall3, syscall4};
    use std::{ffi::CString, os::fd::FromRawFd};
    fn checked(result: usize) -> io::Result<usize> {
        if (result as isize) < 0 {
            Err(io::Error::from_raw_os_error(
                (result as isize)
                    .checked_neg()
                    .and_then(|e| i32::try_from(e).ok())
                    .unwrap_or(5),
            ))
        } else {
            Ok(result)
        }
    }
    fn name(path: &Path) -> io::Result<CString> {
        let bytes = path.as_os_str().as_encoded_bytes();
        if bytes.is_empty() {
            return Err(io::Error::from_raw_os_error(ERRNO_ENOENT));
        }
        if bytes.len() >= PATH_MAX {
            return Err(io::Error::from_raw_os_error(ERRNO_ENAMETOOLONG));
        }
        CString::new(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Path contains NUL"))
    }
    let mode = if let Some(original) = original {
        let original = name(original)?;
        let mut metadata = RawFileMetadata::default();
        // SAFETY: live NUL path and exclusively borrowed repr(C) output; neither
        // pointer is retained by this synchronous metadata syscall.
        checked(unsafe {
            syscall3(
                Syscall::VfsMetadataWithStatus,
                original.as_ptr() as usize,
                &mut metadata as *mut RawFileMetadata as usize,
                1,
            )
        })?;
        if metadata.file_type != scarlet_sys::FILE_TYPE_REGULAR {
            return Err(io::Error::other(
                "Destination changed to a non-regular file",
            ));
        }
        let p = metadata.permissions;
        (if p & scarlet_sys::FILE_PERMISSION_READ != 0 {
            0o400
        } else {
            0
        }) | (if p & scarlet_sys::FILE_PERMISSION_WRITE != 0 {
            0o200
        } else {
            0
        }) | (if p & scarlet_sys::FILE_PERMISSION_EXECUTE != 0 {
            0o100
        } else {
            0
        })
    } else {
        0o600
    };
    let path = name(path)?;
    // SAFETY: ABI flags request one fresh exclusive descriptor; the live path
    // is read-only and the kernel does not retain its pointer.
    let descriptor = checked(unsafe {
        syscall4(
            Syscall::VfsOpenAt,
            CURRENT_DIRECTORY,
            path.as_ptr() as usize,
            (VFS_O_WRONLY | VFS_O_CREAT | VFS_O_EXCL | VFS_O_NOFOLLOW | VFS_O_CLOEXEC) as usize,
            mode,
        )
    })?;
    let descriptor =
        i32::try_from(descriptor).map_err(|_| io::Error::other("Invalid file descriptor"))?;
    // SAFETY: the successful syscall transferred one descriptor; File is its
    // unique owner and closes it exactly once.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusive_creation_preserves_existing_file() {
        let path = std::env::temp_dir().join(format!("vellum-exclusive-{}", std::process::id()));
        std::fs::write(&path, b"original").unwrap();
        assert_eq!(
            create_staging(&path, None).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        std::fs::remove_file(path).unwrap();
    }
}

/// Check that cleanup still addresses the inode owned by this open file.
/// A changed/unknown destination is retained and reported to the user.
#[cfg(all(not(target_os = "scarlet"), unix))]
pub(crate) fn same_file(path: &Path, file: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata()?;
    let current = std::fs::symlink_metadata(path)?;
    Ok(current.is_file() && opened.dev() == current.dev() && opened.ino() == current.ino())
}
#[cfg(all(not(target_os = "scarlet"), not(unix)))]
pub(crate) fn same_file(_path: &Path, _file: &File) -> io::Result<bool> {
    Ok(false)
}
#[cfg(target_os = "scarlet")]
pub(crate) fn same_file(path: &Path, file: &File) -> io::Result<bool> {
    use scarlet_sys::{RawFileMetadata, Syscall, syscall2, syscall3};
    use std::{ffi::CString, os::fd::AsRawFd};
    let path = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Path contains NUL"))?;
    let mut opened = RawFileMetadata::default();
    let mut current = RawFileMetadata::default();
    // SAFETY: File owns the live descriptor; both exclusively borrowed metadata
    // outputs and the NUL path live throughout these synchronous calls.
    let descriptor_result = unsafe {
        syscall2(
            Syscall::FileMetadata,
            file.as_raw_fd() as usize,
            &mut opened as *mut RawFileMetadata as usize,
        )
    };
    let path_result = unsafe {
        syscall3(
            Syscall::VfsMetadataWithStatus,
            path.as_ptr() as usize,
            &mut current as *mut RawFileMetadata as usize,
            1,
        )
    };
    if descriptor_result != 0 || path_result != 0 {
        return Err(io::Error::other("Cannot verify partial output ownership"));
    }
    Ok(opened.file_id != 0
        && current.file_type == scarlet_sys::FILE_TYPE_REGULAR
        && opened.file_id == current.file_id
        && opened.created == current.created)
}
