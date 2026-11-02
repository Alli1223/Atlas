//! On-disk storage for uploaded bytes: attachments, avatars, generated images.
//!
//! # The one rule: paths are built from ids, never from user input
//!
//! Every file lives at `{data_dir}/uploads/{area}/{id}[/{version}]`, where `area` is a
//! compile-time constant and `id`/`version` are server-generated (a UUID, an integer). A
//! user-supplied filename is *display metadata* stored in the database; it is never joined
//! onto a path. So `../../etc/passwd` as a filename is just an odd-looking label, and there is
//! no traversal check to get wrong because there is no traversal to check.
//!
//! [`safe_id`] is still applied to every id before it touches a path — belt and braces against
//! a future caller passing something that is not a UUID.
//!
//! # Streaming, bounded, hashed
//!
//! [`write_stream`] copies a request body to a temp file in the destination directory while
//! counting bytes and hashing, refuses past the limit, and renames into place only on success
//! — so a failed or oversized upload leaves no partial file behind, and a reader never sees a
//! half-written one.

use std::path::{Path, PathBuf};

use axum::body::Body;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::error::{AppError, AppResult};

/// The storage areas. A closed set, so a path's first component is never user-chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    /// Card attachments, versioned.
    Attachments,
    /// User avatars.
    Avatars,
    /// AI-generated images (project covers, card covers, references).
    Generated,
}

impl Area {
    fn dir_name(self) -> &'static str {
        match self {
            Self::Attachments => "attachments",
            Self::Avatars => "avatars",
            Self::Generated => "generated",
        }
    }
}

/// What [`write_stream`] wrote.
#[derive(Debug, Clone)]
pub struct Stored {
    /// Bytes written.
    pub size: u64,
    /// Lowercase hex SHA-256 of the content.
    pub sha256: String,
}

/// Refuses anything that is not a plain `[A-Za-z0-9-]` id.
pub fn safe_id(id: &str) -> AppResult<&str> {
    if !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        Ok(id)
    } else {
        Err(AppError::internal(anyhow::anyhow!(
            "refusing to build a storage path from {id:?}"
        )))
    }
}

/// The root every upload lives under.
pub fn root(data_dir: &Path) -> PathBuf {
    data_dir.join("uploads")
}

/// The path of one stored object: `{root}/{area}/{id}` or `{root}/{area}/{id}/{version}`.
pub fn path_for(data_dir: &Path, area: Area, id: &str, version: Option<i64>) -> AppResult<PathBuf> {
    let mut path = root(data_dir).join(area.dir_name()).join(safe_id(id)?);
    if let Some(version) = version {
        path = path.join(version.to_string());
    }
    Ok(path)
}

/// Streams `body` to `dest`, refusing once it exceeds `limit` bytes.
///
/// Writes to a sibling temp file first and renames on success.
pub async fn write_stream(body: Body, dest: &Path, limit: u64) -> AppResult<Stored> {
    let dir = dest
        .parent()
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("storage path has no parent")))?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(AppError::internal)?;
    let temp = dir.join(format!(".upload-{}", uuid::Uuid::new_v4()));

    let result = copy_bounded(body, &temp, limit).await;
    match result {
        Ok(stored) => {
            tokio::fs::rename(&temp, dest)
                .await
                .map_err(AppError::internal)?;
            Ok(stored)
        }
        Err(err) => {
            let _ = tokio::fs::remove_file(&temp).await;
            Err(err)
        }
    }
}

/// Writes an in-memory buffer through the same temp-then-rename path.
pub async fn write_bytes(bytes: &[u8], dest: &Path) -> AppResult<Stored> {
    write_stream(Body::from(bytes.to_vec()), dest, u64::MAX).await
}

async fn copy_bounded(body: Body, temp: &Path, limit: u64) -> AppResult<Stored> {
    let mut file = tokio::fs::File::create(temp)
        .await
        .map_err(AppError::internal)?;
    let mut hasher = Sha256::new();
    let mut size: u64 = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|err| AppError::BadRequest(format!("upload interrupted: {err}")))?;
        size = size.saturating_add(chunk.len() as u64);
        if size > limit {
            return Err(AppError::Validation(format!(
                "The file is larger than the {} MB upload limit.",
                limit / (1024 * 1024)
            )));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(AppError::internal)?;
    }
    file.flush().await.map_err(AppError::internal)?;
    file.sync_all().await.map_err(AppError::internal)?;
    Ok(Stored {
        size,
        sha256: hex::encode(hasher.finalize()),
    })
}

/// Removes a stored object (a file, or a directory of versions). Missing is fine.
pub async fn remove(path: &Path) {
    let result = if path.is_dir() {
        tokio::fs::remove_dir_all(path).await
    } else {
        tokio::fs::remove_file(path).await
    };
    if let Err(err) = result
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %err, "failed to remove a stored upload");
    }
}

/// Cleans a user-supplied filename into a safe display label.
///
/// Keeps the last path segment, drops control characters and the characters every OS
/// reserves, collapses to at most 200 characters (keeping the extension), and falls back to
/// `file` when nothing is left. Display only — see the module doc.
pub fn clean_filename(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim().to_owned();
    if cleaned.is_empty() {
        return "file".to_owned();
    }
    if cleaned.chars().count() <= 200 {
        return cleaned;
    }
    let (stem, ext) = match cleaned.rsplit_once('.') {
        Some((stem, ext)) if ext.chars().count() <= 16 => (stem.to_owned(), format!(".{ext}")),
        _ => (cleaned.clone(), String::new()),
    };
    let keep = 200usize.saturating_sub(ext.chars().count());
    format!("{}{ext}", stem.chars().take(keep).collect::<String>())
}

/// Picks a content type from what the client declared and the file's extension.
///
/// The declared type is not trusted for anything that matters (see [`is_inline_safe`]) —
/// this only gives a download a sensible default.
pub fn content_type_for(declared: Option<&str>, filename: &str) -> String {
    if let Some(declared) = declared.map(str::trim).filter(|d| {
        !d.is_empty()
            && d.len() <= 127
            && d.contains('/')
            && d.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
            && *d != "application/x-www-form-urlencoded"
    }) {
        return declared
            .split(';')
            .next()
            .unwrap_or(declared)
            .trim()
            .to_ascii_lowercase();
    }
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "md" | "log" => "text/plain",
        "json" => "application/json",
        "csv" => "text/csv",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
    .to_owned()
}

/// Whether a stored type may be shown inline in the browser (image/PDF preview).
///
/// An allowlist of formats a browser renders without executing anything. `image/svg+xml` is
/// conspicuously absent: an SVG is a document that can carry script, so it downloads instead.
/// Everything not on the list is served `Content-Disposition: attachment` as
/// `application/octet-stream`, so an uploaded `.html` is a file to save, never a page on
/// Atlas's origin.
pub fn is_inline_safe(content_type: &str) -> bool {
    matches!(
        content_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "application/pdf"
    )
}

/// Sniffs the magic bytes of the common image formats. Used where an upload *must* be an
/// image (avatars), so the declared content type cannot smuggle something else in.
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_that_are_not_plain_tokens_never_reach_a_path() {
        assert!(safe_id("0192f0c1-aaaa-7bbb-8ccc-1234567890ab").is_ok());
        for bad in [
            "",
            "..",
            "../x",
            "a/b",
            "a\\b",
            "a b",
            "a.b",
            &"a".repeat(65),
        ] {
            assert!(safe_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn filenames_are_display_labels_with_the_path_and_junk_stripped() {
        assert_eq!(clean_filename("model.blend"), "model.blend");
        assert_eq!(clean_filename("../../etc/passwd"), "passwd");
        assert_eq!(clean_filename("C:\\Users\\me\\cv.pdf"), "cv.pdf");
        assert_eq!(clean_filename("a\u{0000}b<>.txt"), "ab.txt");
        assert_eq!(clean_filename("..."), "file");
        assert_eq!(clean_filename(""), "file");
        let long = format!("{}.blend", "x".repeat(300));
        let cleaned = clean_filename(&long);
        assert_eq!(cleaned.chars().count(), 200);
        assert_eq!(cleaned.rsplit_once('.').map(|(_, ext)| ext), Some("blend"));
    }

    #[test]
    fn only_passive_formats_render_inline() {
        assert!(is_inline_safe("image/png"));
        assert!(is_inline_safe("application/pdf"));
        for risky in [
            "image/svg+xml",
            "text/html",
            "application/javascript",
            "text/plain",
        ] {
            assert!(!is_inline_safe(risky), "{risky}");
        }
    }

    #[test]
    fn content_types_fall_back_to_the_extension() {
        assert_eq!(
            content_type_for(Some("image/png; charset=x"), "a"),
            "image/png"
        );
        assert_eq!(content_type_for(None, "photo.JPG"), "image/jpeg");
        assert_eq!(
            content_type_for(Some(""), "a.unknown"),
            "application/octet-stream"
        );
    }

    #[test]
    fn image_sniffing_reads_magic_bytes_not_names() {
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff_image(b"\xff\xd8\xff\xe0"), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF89a.."), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"<svg xmlns="), None);
    }

    #[tokio::test]
    async fn an_oversized_stream_leaves_nothing_behind() {
        let dir = std::env::temp_dir().join(format!("atlas-storage-{}", uuid::Uuid::new_v4()));
        let dest = dir.join("obj");
        let err = write_stream(Body::from(vec![0u8; 2048]), &dest, 1024)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
        assert!(!dest.exists());
        let leftovers = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(leftovers, 0, "the temp file must be removed");

        let stored = write_stream(Body::from(b"hello".to_vec()), &dest, 1024)
            .await
            .unwrap();
        assert_eq!(stored.size, 5);
        assert_eq!(
            stored.sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
