//! Where things are, in the protocol's terms: file URIs and line
//! columns in the encoding the server chose.

use std::path::{Path, PathBuf};

use url::Url;

/// How a server counts columns within a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Encoding {
    Utf8,
    Utf16,
}

/// The `file://` URI of an absolute path.
pub(super) fn uri(path: &Path) -> Option<String> {
    Url::from_file_path(path).ok().map(String::from)
}

/// The path a `file://` URI names.
pub(super) fn path(uri: &str) -> Option<PathBuf> {
    Url::parse(uri).ok()?.to_file_path().ok()
}

/// The column of byte offset `byte` within `line`, counted in
/// `encoding`'s code units.
pub(super) fn character(line: &str, byte: usize, encoding: Encoding) -> usize {
    let prefix = line.get(..byte).unwrap_or(line);
    match encoding {
        Encoding::Utf8 => prefix.len(),
        Encoding::Utf16 => prefix.encode_utf16().count(),
    }
}
