//! Existing PNGs use staged replacement; new paths use exclusive creation.
use crate::{Bitmap, DocumentError};
use image::ImageEncoder;
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) fn save_png(bitmap: &Bitmap, path: &Path) -> Result<(), DocumentError> {
    staged(path, |file| {
        image::codecs::png::PngEncoder::new(&mut *file)
            .write_image(
                bitmap.rgba(),
                bitmap.width(),
                bitmap.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(io::Error::other)?;
        file.sync_all()
    })
    .map_err(|e| DocumentError::new(format!("could not save PNG: {e}")))
}
fn staged(path: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let target = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path)?,
        Ok(_) => path.to_path_buf(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(e),
    };
    let metadata = match fs::metadata(&target) {
        Ok(meta) if meta.is_file() => Some(meta),
        Ok(_) => return Err(io::Error::other("destination is not a regular file")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if metadata.is_none() {
        return write_new(&target, write);
    }
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = None;
    for _ in 0..128 {
        let candidate = parent.join(format!(
            ".vellum-save-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match crate::native_fs::create_staging(
            &candidate,
            metadata.as_ref().map(|_| target.as_path()),
        ) {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    let (temporary, mut file) =
        temporary.ok_or_else(|| io::Error::other("cannot create temporary PNG"))?;
    let result = (|| {
        write(&mut file)?;
        drop(file);
        fs::rename(&temporary, &target)
    })();
    match result {
        Ok(()) => Ok(()),
        Err(error) => match fs::remove_file(&temporary) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(io::Error::other(format!(
                "{error}; could not remove staging file {}: {cleanup}",
                temporary.display()
            ))),
        },
    }
}
fn write_new(path: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    // Reserve the exact name before encoding. A file created after the initial
    // missing-destination observation cannot be replaced by publication.
    let mut file = crate::native_fs::create_staging(path, None)?;
    match write(&mut file) {
        Ok(()) => Ok(()),
        Err(error) => {
            let owned = crate::native_fs::same_file(path, &file);
            drop(file);
            match owned {
                Ok(true) => match fs::remove_file(path) {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(io::Error::other(format!(
                        "{error}; partial PNG remains at {}: {cleanup}",
                        path.display()
                    ))),
                },
                _ => Err(io::Error::other(format!(
                    "{error}; destination changed or ownership could not be verified; inspect {}",
                    path.display()
                ))),
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn failed_write_keeps_original_and_cleans_staging() {
        let dir = std::env::temp_dir().join(format!("vellum-failed-save-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("original.png");
        fs::write(&path, b"original").unwrap();
        assert!(
            staged(&path, |file| {
                file.write_all(b"partial")?;
                Err(io::Error::other("disk full"))
            })
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn png_round_trip_and_directory_error() {
        let dir = std::env::temp_dir().join(format!("vellum-png-save-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("edited.png");
        let bitmap = Bitmap::from_rgba(2, 1, vec![255, 0, 0, 128, 0, 255, 0, 255]).unwrap();
        save_png(&bitmap, &path).unwrap();
        let decoded = image::open(&path).unwrap().into_rgba8();
        assert_eq!(decoded.as_raw(), bitmap.rgba());
        assert!(save_png(&bitmap, &dir).is_err());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn save_through_symlink_preserves_link_and_target_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = std::env::temp_dir().join(format!("vellum-symlink-save-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("target.png");
        let link = dir.join("link.png");
        fs::write(&target, b"old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        symlink("target.png", &link).unwrap();
        save_png(&Bitmap::from_rgba(1, 1, vec![1, 2, 3, 255]).unwrap(), &link).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(
            image::open(target).unwrap().into_rgba8().as_raw(),
            &[1, 2, 3, 255]
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn intervening_new_destination_is_never_overwritten() {
        let dir = std::env::temp_dir().join(format!("vellum-raced-save-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("new.png");
        assert!(!path.exists());
        // This is the race between staged()'s initial stat and write_new's open.
        fs::write(&path, b"another writer").unwrap();
        assert_eq!(
            write_new(&path, |_| panic!("must not encode into another file"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&path).unwrap(), b"another writer");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn new_write_failure_cleans_owned_file_but_preserves_replacement() {
        let dir = std::env::temp_dir().join(format!("vellum-new-save-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("new.png");
        assert!(
            write_new(&path, |f| {
                f.write_all(b"partial")?;
                Err(io::Error::other("disk full"))
            })
            .is_err()
        );
        assert!(!path.exists());
        let error = write_new(&path, |f| {
            f.write_all(b"partial")?;
            fs::remove_file(&path)?;
            fs::write(&path, b"replacement")?;
            Err(io::Error::other("disk full"))
        })
        .unwrap_err();
        assert!(error.to_string().contains(path.to_str().unwrap()));
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn staging_cleanup_failure_reports_exact_path() {
        let dir = std::env::temp_dir().join(format!("vellum-cleanup-save-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("original.png");
        fs::write(&target, b"original").unwrap();
        let mut blocked = None;
        let error = staged(&target, |file| {
            file.write_all(b"partial")?;
            let path = fs::read_dir(&dir)?
                .map(|e| e.unwrap().path())
                .find(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".vellum-save-")
                })
                .unwrap();
            fs::remove_file(&path)?;
            fs::create_dir(&path)?;
            blocked = Some(path);
            Err(io::Error::other("injected write failure"))
        })
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains(blocked.unwrap().to_str().unwrap())
        );
        assert_eq!(fs::read(target).unwrap(), b"original");
        fs::remove_dir_all(dir).unwrap();
    }
}
