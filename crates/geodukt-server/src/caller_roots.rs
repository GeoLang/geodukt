use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use geodukt_core::incremental::hash_bytes;
use geodukt_core::manifest::Manifest;

// both lengths and the character rule match caller_directory_name in geolang
const READABLE_SUBJECT_LENGTH: usize = 64;
const SUBJECT_DIGEST_LENGTH: usize = 32;
const UNSAFE_SUBJECT_CHARACTER_REPLACEMENT: char = '_';

pub fn caller_directory_name(subject: &str) -> String {
    let readable: String = subject
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                UNSAFE_SUBJECT_CHARACTER_REPLACEMENT
            }
        })
        .take(READABLE_SUBJECT_LENGTH)
        .collect();
    let digest = hash_bytes(subject.as_bytes());
    format!("{readable}-{}", &digest[..SUBJECT_DIGEST_LENGTH])
}

#[derive(Debug, Clone, Default)]
pub struct CallerRoots(Vec<PathBuf>);

impl CallerRoots {
    pub fn new(roots: &[PathBuf]) -> std::io::Result<Self> {
        roots
            .iter()
            .map(|root| {
                root.canonicalize().map_err(|error| {
                    std::io::Error::new(
                        error.kind(),
                        format!("caller root '{}': {error}", root.display()),
                    )
                })
            })
            .collect::<std::io::Result<_>>()
            .map(Self)
    }

    pub fn confine(
        &self,
        manifest: &mut Manifest,
        subject: Option<&str>,
    ) -> Result<(), (StatusCode, String)> {
        if self.0.is_empty() {
            return Ok(());
        }
        let Some(subject) = subject else {
            return Err((
                StatusCode::UNAUTHORIZED,
                "a verified caller is required to run a manifest that names files".to_string(),
            ));
        };
        let directory_name = caller_directory_name(subject);
        let caller_directories: Vec<PathBuf> = self
            .0
            .iter()
            .map(|root| root.join(&directory_name))
            .collect();

        let sources = manifest
            .source
            .iter_mut()
            .map(|source| ("source", &source.name, &mut source.path));
        let sinks = manifest
            .sink
            .iter_mut()
            .map(|sink| ("sink", &sink.name, &mut sink.path));
        for (kind, name, path) in sources.chain(sinks) {
            let confined = resolve(Path::new(path.as_str()))
                .filter(|resolved| {
                    caller_directories
                        .iter()
                        .any(|directory| resolved.starts_with(directory))
                })
                .and_then(|resolved| resolved.into_os_string().into_string().ok())
                .ok_or_else(|| {
                    (
                        StatusCode::FORBIDDEN,
                        format!(
                            "{kind} '{name}' names '{path}', which is outside your own directory"
                        ),
                    )
                })?;
            // the run opens the path that was checked, not the given one resolved again
            *path = confined;
        }
        Ok(())
    }
}

// a sink may not exist yet
fn resolve(path: &Path) -> Option<PathBuf> {
    let absolute = std::path::absolute(path).ok()?;
    let mut existing = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        // a dangling symlink passes lstat and fails canonicalize
        if existing.symlink_metadata().is_ok() {
            let mut resolved = existing.canonicalize().ok()?;
            resolved.extend(missing.iter().rev());
            return Some(resolved);
        }
        // a trailing `..` has no file name and is refused
        missing.push(existing.file_name()?);
        existing = existing.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // expected names computed with caller_directory_name in geolang
    #[test]
    fn caller_directory_name_matches_geolang() {
        let pairs = [
            ("user-42", "user-42-6d894aa3ee802549d7f340e7c1cf0d1c"),
            (
                "auth0|abc/def:ghi",
                "auth0_abc_def_ghi-c5f49df3511e4d63ac2b5e78e3474d9a",
            ),
            (
                "a-very-long-subject-name-that-goes-on-and-on-past-the-sixty-four-character-limit@example.com",
                "a-very-long-subject-name-that-goes-on-and-on-past-the-sixty-four-ef10b24a6794d8d9af78ff82fd80bed5",
            ),
            // one replacement per character, not per byte
            ("é/ü", "___-172f7ec3f2c56f87a09231f4aa5b774b"),
        ];
        for (subject, expected) in pairs {
            assert_eq!(caller_directory_name(subject), expected, "{subject}");
        }
    }
}
