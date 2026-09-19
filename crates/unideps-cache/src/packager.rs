use anyhow::{Context, Result};
use std::fs::File;
use std::path::{Path, PathBuf};

pub struct Packager;

fn temp_sibling(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    path.with_file_name(format!(".{name}.tmp-{}", std::process::id()))
}

impl Packager {
    /// Packs `src_dir` into a `.tar.zst`. The archive is written to a temporary file
    /// and renamed into place only after the zstd frame is fully finished, so an
    /// interrupted run never leaves a truncated archive under the final name.
    pub fn pack_dir_to_zst(src_dir: impl AsRef<Path>, out_tar_zst: impl AsRef<Path>) -> Result<()> {
        let out = out_tar_zst.as_ref();
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = temp_sibling(out);
        let result = (|| -> Result<()> {
            let file = File::create(&tmp)?;
            let enc = zstd::Encoder::new(file, 3)?;
            let mut tar_builder = tar::Builder::new(enc);
            // Keep symlinks (libfoo.so -> libfoo.so.1) as links instead of duplicating files.
            tar_builder.follow_symlinks(false);
            tar_builder.append_dir_all(".", src_dir.as_ref())?;
            let enc = tar_builder.into_inner()?;
            let file = enc.finish()?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(e) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(e).with_context(|| format!("Failed to create archive {}", out.display()));
        }
        std::fs::rename(&tmp, out).with_context(|| format!("Failed to move archive into place: {}", out.display()))?;
        Ok(())
    }

    pub fn unpack_zst_to_dir(tar_zst: impl AsRef<Path>, dst_dir: impl AsRef<Path>) -> Result<()> {
        let file = File::open(tar_zst.as_ref())?;
        let dec = zstd::Decoder::new(file)?;
        let mut archive = tar::Archive::new(dec);
        archive
            .unpack(dst_dir.as_ref())
            .with_context(|| format!("Failed to unpack {}", tar_zst.as_ref().display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_no_partial_archive() {
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("src");
        std::fs::create_dir_all(src.join("include/sub")).unwrap();
        std::fs::write(src.join("include/sub/a.h"), b"int a;").unwrap();
        std::fs::write(src.join("lib.a"), vec![7u8; 100_000]).unwrap();

        let archive = temp.path().join("cache/pkg.tar.zst");
        Packager::pack_dir_to_zst(&src, &archive).unwrap();
        assert!(archive.exists());
        let leftovers: Vec<_> = std::fs::read_dir(archive.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());

        let dst = temp.path().join("dst");
        Packager::unpack_zst_to_dir(&archive, &dst).unwrap();
        assert_eq!(std::fs::read(dst.join("include/sub/a.h")).unwrap(), b"int a;");
        assert_eq!(std::fs::read(dst.join("lib.a")).unwrap().len(), 100_000);
    }

    #[test]
    fn truncated_archive_fails_to_unpack() {
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("big.bin"), (0..200_000u32).flat_map(|i| i.to_le_bytes()).collect::<Vec<_>>()).unwrap();
        let archive = temp.path().join("a.tar.zst");
        Packager::pack_dir_to_zst(&src, &archive).unwrap();
        let bytes = std::fs::read(&archive).unwrap();
        std::fs::write(&archive, &bytes[..bytes.len() / 2]).unwrap();
        assert!(Packager::unpack_zst_to_dir(&archive, temp.path().join("dst")).is_err());
    }
}
