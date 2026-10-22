//! Which language server runs for which language: built-in defaults
//! overlaid by `[lsp.<language>]` tables in `config.toml`.

use std::collections::HashMap;
use std::path::Path;

use deltoids::Language;
use serde::Deserialize;
use serde_json::{Value, json};

/// How to start one language server: a program and its arguments, and
/// the settings it starts with.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ServerSpec {
    pub(crate) command: Vec<String>,
    pub(crate) settings: Option<ServerSettings>,
}

/// Options sent as `initializationOptions` and as the answer to
/// `workspace/configuration` for `section`. Kept as JSON text so a
/// spec stays hashable.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ServerSettings {
    pub(crate) section: String,
    options: String,
}

impl ServerSettings {
    fn new(section: &str, options: Value) -> Self {
        Self {
            section: section.to_string(),
            options: options.to_string(),
        }
    }

    pub(crate) fn options(&self) -> Value {
        serde_json::from_str(&self.options).unwrap_or(Value::Null)
    }
}

/// rust-analyzer builds in its own target directory and skips the
/// startup `cargo check`, so it never holds the build lock the user's
/// `cargo build` and `cargo test` need.
fn rust_analyzer_settings() -> ServerSettings {
    ServerSettings::new(
        "rust-analyzer",
        json!({ "cargo": { "targetDir": true }, "checkOnSave": false }),
    )
}

impl ServerSpec {
    fn new(command: &[&str]) -> Self {
        Self {
            command: command.iter().map(|part| part.to_string()).collect(),
            settings: None,
        }
    }

    fn with_settings(mut self, settings: ServerSettings) -> Self {
        self.settings = Some(settings);
        self
    }

    /// The program's file name, for messages ("rust-analyzer").
    pub(crate) fn name(&self) -> String {
        let program = self.command.first().map_or("", String::as_str);
        Path::new(program)
            .file_name()
            .map_or(program.to_string(), |name| {
                name.to_string_lossy().into_owned()
            })
    }
}

/// The server for each language that has one.
#[derive(Debug, Clone)]
pub(crate) struct ServerTable {
    servers: HashMap<Language, ServerSpec>,
}

impl ServerTable {
    /// The built-in defaults: the usual server for each language.
    pub(crate) fn defaults() -> Self {
        let typescript = ServerSpec::new(&["typescript-language-server", "--stdio"]);
        let clangd = ServerSpec::new(&["clangd"]);
        let servers = [
            (
                Language::Rust,
                ServerSpec::new(&["rust-analyzer"]).with_settings(rust_analyzer_settings()),
            ),
            (Language::Go, ServerSpec::new(&["gopls"])),
            (Language::TypeScript, typescript.clone()),
            (Language::Tsx, typescript.clone()),
            (Language::JavaScript, typescript),
            (
                Language::Python,
                ServerSpec::new(&["pyright-langserver", "--stdio"]),
            ),
            (Language::C, clangd.clone()),
            (Language::Cpp, clangd),
            (Language::Ruby, ServerSpec::new(&["ruby-lsp"])),
            (Language::Lua, ServerSpec::new(&["lua-language-server"])),
            (
                Language::Bash,
                ServerSpec::new(&["bash-language-server", "start"]),
            ),
            (Language::Java, ServerSpec::new(&["jdtls"])),
            (
                Language::Kotlin,
                ServerSpec::new(&["kotlin-language-server"]),
            ),
        ];
        Self {
            servers: servers.into_iter().collect(),
        }
    }

    /// The defaults overlaid by the user's `config.toml`. A missing or
    /// unreadable file keeps the defaults.
    pub(crate) fn load() -> Self {
        let text = deltoids::config::config_file_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        Self::parse(&text)
    }

    /// The defaults overlaid by the `[lsp.<language>]` tables of a
    /// `config.toml` body. `command` replaces a server's program and
    /// keeps its settings, `enabled = false` removes it. Unknown languages and empty commands are ignored, and
    /// a file that does not parse keeps the defaults.
    pub(crate) fn parse(text: &str) -> Self {
        let mut table = Self::defaults();
        let Ok(config) = toml::from_str::<Config>(text) else {
            return table;
        };
        for (id, raw) in config.lsp {
            let Some(language) = Language::from_id(&id) else {
                continue;
            };
            if raw.enabled == Some(false) {
                table.servers.remove(&language);
                continue;
            }
            if let Some(command) = raw.command.filter(|command| !command.is_empty()) {
                let settings = table
                    .servers
                    .remove(&language)
                    .and_then(|spec| spec.settings);
                table
                    .servers
                    .insert(language, ServerSpec { command, settings });
            }
        }
        table
    }

    pub(crate) fn get(&self, language: Language) -> Option<&ServerSpec> {
        self.servers.get(&language)
    }
}

#[derive(Debug, Default, Deserialize)]
struct Config {
    #[serde(default)]
    lsp: HashMap<String, RawServer>,
}

#[derive(Debug, Deserialize)]
struct RawServer {
    command: Option<Vec<String>>,
    enabled: Option<bool>,
}

/// The LSP `languageId` of a language.
pub(crate) fn language_id(language: Language) -> &'static str {
    match language {
        Language::Tsx => "typescriptreact",
        Language::Bash => "shellscript",
        other => other.id(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(table: &ServerTable, language: Language) -> Option<Vec<String>> {
        table.get(language).map(|spec| spec.command.clone())
    }

    #[test]
    fn defaults_cover_common_languages_and_share_the_typescript_server() {
        let table = ServerTable::parse("");
        assert_eq!(
            command(&table, Language::Rust),
            Some(vec!["rust-analyzer".to_string()])
        );
        assert_eq!(table.get(Language::Tsx), table.get(Language::JavaScript));
        assert_eq!(command(&table, Language::Json), None);
    }

    #[test]
    fn config_replaces_and_disables_servers() {
        let table = ServerTable::parse(
            r#"
            [lsp.python]
            command = ["basedpyright-langserver", "--stdio"]
            [lsp.go]
            enabled = false
            [lsp.klingon]
            command = ["qapla"]
            [lsp.rust]
            command = []
            "#,
        );
        assert_eq!(
            command(&table, Language::Python),
            Some(vec![
                "basedpyright-langserver".to_string(),
                "--stdio".to_string()
            ])
        );
        assert_eq!(command(&table, Language::Go), None);
        assert_eq!(
            command(&table, Language::Rust),
            Some(vec!["rust-analyzer".to_string()])
        );
    }

    #[test]
    fn replacing_the_rust_command_keeps_the_rust_analyzer_settings() {
        let defaults = ServerTable::defaults();
        let table = ServerTable::parse(
            "[lsp.rust]\ncommand = [\"rustup\", \"run\", \"stable\", \"rust-analyzer\"]\n",
        );
        let rust = table.get(Language::Rust).unwrap();
        assert_eq!(rust.command[0], "rustup");
        assert!(rust.settings.is_some());
        assert_eq!(
            rust.settings,
            defaults.get(Language::Rust).unwrap().settings
        );
        assert_eq!(table.get(Language::Go).unwrap().settings, None);
    }

    #[test]
    fn an_unparsable_config_keeps_the_defaults() {
        assert!(ServerTable::parse("[lsp").get(Language::Go).is_some());
    }

    #[test]
    fn names_come_from_the_program_file_name() {
        let spec = ServerSpec::new(&["/opt/bin/gopls", "serve"]);
        assert_eq!(spec.name(), "gopls");
    }
}
