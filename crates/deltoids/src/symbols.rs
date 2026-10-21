//! The symbols on a line: what a reader can point at to ask where it is
//! defined or who calls it.

use std::ops::Range;

use crate::Language;
use crate::syntax::ParsedFile;

/// A parsed source file, answering which identifiers sit on each line.
pub struct Symbols {
    parsed: Option<ParsedFile>,
}

impl Symbols {
    /// Parse `source` as `language`. A source that does not parse has no
    /// symbols.
    pub fn parse(language: Language, source: &str) -> Self {
        Symbols {
            parsed: ParsedFile::parse_as(language, source),
        }
    }

    /// Byte ranges, within the line, of the identifiers on 1-based
    /// `line`, in source order. Empty past the end of the file.
    pub fn on_line(&self, line: usize) -> Vec<Range<usize>> {
        match (&self.parsed, line.checked_sub(1)) {
            (Some(parsed), Some(index)) => parsed.identifiers_on_line(index),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(language: Language, source: &str, line: usize) -> Vec<String> {
        let text = source.lines().nth(line - 1).unwrap_or_default();
        Symbols::parse(language, source)
            .on_line(line)
            .into_iter()
            .map(|range| text[range].to_string())
            .collect()
    }

    #[test]
    fn rust_calls_methods_and_paths_are_symbols_but_keywords_are_not() {
        let source = "fn main() {\n    let total = cart::Cart::new().total(1);\n}\n";
        assert_eq!(
            names(Language::Rust, source, 2),
            ["total", "cart", "Cart", "new", "total"]
        );
    }

    #[test]
    fn typescript_property_access_names_each_part() {
        let source = "const x = order.items.map(price);\n";
        assert_eq!(
            names(Language::TypeScript, source, 1),
            ["x", "order", "items", "map", "price"]
        );
    }

    #[test]
    fn python_definition_line_names_the_function_and_parameters() {
        let source = "def line_price(qty, unit):\n    return qty * unit\n";
        assert_eq!(
            names(Language::Python, source, 1),
            ["line_price", "qty", "unit"]
        );
    }

    #[test]
    fn ranges_are_byte_offsets_after_multibyte_text() {
        let source = "fn main() {\n    let é = \"ñ\"; go(é);\n}\n";
        assert_eq!(names(Language::Rust, source, 2), ["é", "go", "é"]);
    }

    #[test]
    fn lines_without_identifiers_or_past_the_end_are_empty() {
        let source = "fn main() {\n}\n";
        assert!(names(Language::Rust, source, 2).is_empty());
        assert!(Symbols::parse(Language::Rust, source).on_line(9).is_empty());
        assert!(Symbols::parse(Language::Rust, source).on_line(0).is_empty());
    }
}
