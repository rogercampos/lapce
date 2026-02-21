use std::{path::Path, rc::Rc};

use floem::{
    keyboard::Modifiers,
    peniko::kurbo::Rect,
    reactive::{RwSignal, Scope, SignalGet, SignalUpdate},
};
use lapce_core::{
    command::FocusCommand, language::LapceLanguage, movement::Movement,
};
use lsp_types::Location;

use crate::{
    command::{CommandExecuted, CommandKind, InternalCommand},
    editor::location::{EditorLocation, EditorPosition},
    keypress::{KeyPressFocus, condition::Condition},
    lsp::path_from_url,
    workspace_data::{CommonData, Focus},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DefinitionPickerStatus {
    Inactive,
    Active,
}

#[derive(Clone, Debug)]
pub struct DefinitionPickerItem {
    pub location: Location,
    pub display_path: String,
    pub line_number: u32,
}

#[derive(Clone, Debug)]
pub struct DefinitionPickerData {
    pub status: RwSignal<DefinitionPickerStatus>,
    pub active: RwSignal<usize>,
    pub offset: usize,
    pub items: Vec<DefinitionPickerItem>,
    pub layout_rect: Rect,
    pub common: Rc<CommonData>,
}

impl KeyPressFocus for DefinitionPickerData {
    fn check_condition(&self, condition: Condition) -> bool {
        matches!(condition, Condition::ListFocus | Condition::ModalFocus)
    }

    fn run_command(
        &self,
        command: &crate::command::LapceCommand,
        _count: Option<usize>,
        _mods: Modifiers,
    ) -> CommandExecuted {
        match &command.kind {
            CommandKind::Focus(cmd) => {
                self.run_focus_command(cmd);
            }
            _ => {}
        }
        CommandExecuted::Yes
    }

    fn receive_char(&self, _c: &str) {}
}

impl DefinitionPickerData {
    pub fn new(cx: Scope, common: Rc<CommonData>) -> Self {
        let status = cx.create_rw_signal(DefinitionPickerStatus::Inactive);

        let picker = Self {
            status,
            active: cx.create_rw_signal(0),
            offset: 0,
            items: Vec::new(),
            layout_rect: Rect::ZERO,
            common,
        };

        {
            let picker = picker.clone();
            cx.create_effect(move |_| {
                let focus = picker.common.focus.get();
                if focus != Focus::DefinitionPicker
                    && picker.status.get_untracked()
                        != DefinitionPickerStatus::Inactive
                {
                    picker.cancel();
                }
            })
        }

        picker
    }

    pub fn show(
        &mut self,
        locations: Vec<Location>,
        offset: usize,
        language: LapceLanguage,
    ) {
        let workspace_path = self.common.workspace.path.clone();

        self.active.set(0);
        self.offset = offset;
        // LSP positions use u32 for line/character, so they are always non-negative.
        // No additional validation is needed beyond what the LSP protocol guarantees.
        self.items = locations
            .into_iter()
            .map(|location| {
                let path = path_from_url(&location.uri);
                let display_path =
                    format_display_path(&path, workspace_path.as_deref(), language);
                let line_number = location.range.start.line + 1;
                DefinitionPickerItem {
                    location,
                    display_path,
                    line_number,
                }
            })
            .collect();
        self.status.set(DefinitionPickerStatus::Active);
        self.common.focus.set(Focus::DefinitionPicker);
    }

    fn cancel(&self) {
        self.status.set(DefinitionPickerStatus::Inactive);
        if let Focus::DefinitionPicker = self.common.focus.get_untracked() {
            self.common.focus.set(Focus::Workbench);
        }
    }

    pub fn select(&self) {
        if let Some(item) = self.items.get(self.active.get_untracked()) {
            let location = EditorLocation {
                path: path_from_url(&item.location.uri),
                position: Some(EditorPosition::Position(item.location.range.start)),
                scroll_offset: None,
                same_editor_tab: false,
            };
            self.common
                .internal_command
                .send(InternalCommand::JumpToLocation { location });
        }
        self.cancel();
    }

    pub fn next(&self) {
        let active = self.active.get_untracked();
        let new = Movement::Down.update_index(active, self.items.len(), 1, true);
        self.active.set(new);
    }

    pub fn previous(&self) {
        let active = self.active.get_untracked();
        let new = Movement::Up.update_index(active, self.items.len(), 1, true);
        self.active.set(new);
    }

    fn run_focus_command(&self, cmd: &FocusCommand) -> CommandExecuted {
        match cmd {
            FocusCommand::ModalClose => {
                self.cancel();
            }
            FocusCommand::ListNext => {
                self.next();
            }
            FocusCommand::ListPrevious => {
                self.previous();
            }
            FocusCommand::ListSelect => {
                self.select();
            }
            _ => return CommandExecuted::No,
        }
        CommandExecuted::Yes
    }
}

/// Formats a file path for display in the definition picker.
///
/// For Ruby, detects installation paths from all major version managers and shortens them:
/// - Gem paths → `(ruby <ver> / <gem>) <rest>`
/// - Stdlib paths → `(ruby <ver>) <rest>`
///
/// For other languages, uses workspace-relative paths or absolute paths as-is.
fn format_display_path(
    path: &Path,
    workspace_path: Option<&Path>,
    language: LapceLanguage,
) -> String {
    let path_str = path.to_string_lossy();

    if language == LapceLanguage::Ruby {
        if let Some(result) = format_ruby_path(&path_str) {
            return result;
        }
    }

    if let Some(wp) = workspace_path {
        path.strip_prefix(wp)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string()
    } else {
        path_str.to_string()
    }
}

/// Attempts to detect and format a Ruby-related path.
///
/// Handles two main categories:
/// 1. Paths with a known Ruby version root (rbenv, mise, asdf, rvm, chruby,
///    frum, Homebrew Cellar, macOS system Ruby) — extracts version, then
///    looks for gem or stdlib sub-paths.
/// 2. Paths with gems outside the Ruby root (RVM GEM_HOME, Homebrew shared
///    gems, Debian system gems, user ~/.gem) — extracts version from the gem
///    cache path itself.
///
/// ```
/// use lapce_app::definition_picker::format_ruby_path;
///
/// // rbenv gem
/// assert_eq!(
///     format_ruby_path("/home/user/.rbenv/versions/3.2.0/lib/ruby/gems/3.2.0/gems/activesupport-7.0.4/lib/foo.rb"),
///     Some("(ruby 3.2.0 / activesupport-7.0.4) lib/foo.rb".into()),
/// );
///
/// // rbenv stdlib
/// assert_eq!(
///     format_ruby_path("/home/user/.rbenv/versions/3.2.0/lib/ruby/3.2.0/uri/common.rb"),
///     Some("(ruby 3.2.0) uri/common.rb".into()),
/// );
///
/// // mise gem
/// assert_eq!(
///     format_ruby_path("/home/user/.local/share/mise/installs/ruby/3.4.2/lib/ruby/gems/3.4.0/gems/uri-1.1.1/lib/uri/common.rb"),
///     Some("(ruby 3.4.2 / uri-1.1.1) lib/uri/common.rb".into()),
/// );
///
/// // mise stdlib
/// assert_eq!(
///     format_ruby_path("/home/user/.local/share/mise/installs/ruby/3.4.2/lib/ruby/3.4.0/uri/common.rb"),
///     Some("(ruby 3.4.2) uri/common.rb".into()),
/// );
///
/// // asdf gem
/// assert_eq!(
///     format_ruby_path("/home/user/.asdf/installs/ruby/3.3.0/lib/ruby/gems/3.3.0/gems/rack-2.2.7/lib/rack.rb"),
///     Some("(ruby 3.3.0 / rack-2.2.7) lib/rack.rb".into()),
/// );
///
/// // rvm rubies stdlib
/// assert_eq!(
///     format_ruby_path("/home/user/.rvm/rubies/ruby-3.3.0/lib/ruby/3.3.0/net/http.rb"),
///     Some("(ruby 3.3.0) net/http.rb".into()),
/// );
///
/// // rvm GEM_HOME (gems outside Ruby root)
/// assert_eq!(
///     format_ruby_path("/home/user/.rvm/gems/ruby-3.3.0/gems/rails-7.0.0/lib/rails.rb"),
///     Some("(ruby 3.3.0 / rails-7.0.0) lib/rails.rb".into()),
/// );
///
/// // rvm gemset
/// assert_eq!(
///     format_ruby_path("/home/user/.rvm/gems/ruby-3.3.0@myapp/gems/puma-6.0.0/lib/puma.rb"),
///     Some("(ruby 3.3.0 / puma-6.0.0) lib/puma.rb".into()),
/// );
///
/// // chruby / ruby-install
/// assert_eq!(
///     format_ruby_path("/home/user/.rubies/ruby-3.3.0/lib/ruby/gems/3.3.0/gems/json-2.7.0/lib/json.rb"),
///     Some("(ruby 3.3.0 / json-2.7.0) lib/json.rb".into()),
/// );
///
/// // frum gem
/// assert_eq!(
///     format_ruby_path("/home/user/.frum/versions/3.3.0/lib/ruby/gems/3.3.0/gems/minitest-5.20.0/lib/minitest.rb"),
///     Some("(ruby 3.3.0 / minitest-5.20.0) lib/minitest.rb".into()),
/// );
///
/// // Homebrew Cellar gem (macOS Apple Silicon)
/// assert_eq!(
///     format_ruby_path("/opt/homebrew/Cellar/ruby/3.3.0/lib/ruby/gems/3.3.0/gems/bigdecimal-3.1.4/lib/bigdecimal.rb"),
///     Some("(ruby 3.3.0 / bigdecimal-3.1.4) lib/bigdecimal.rb".into()),
/// );
///
/// // Homebrew shared gems
/// assert_eq!(
///     format_ruby_path("/opt/homebrew/lib/ruby/gems/3.3.0/gems/nokogiri-1.15.0/lib/nokogiri.rb"),
///     Some("(ruby 3.3.0 / nokogiri-1.15.0) lib/nokogiri.rb".into()),
/// );
///
/// // Debian system gems
/// assert_eq!(
///     format_ruby_path("/var/lib/gems/3.1.0/gems/bundler-2.4.0/lib/bundler.rb"),
///     Some("(ruby 3.1.0 / bundler-2.4.0) lib/bundler.rb".into()),
/// );
///
/// // User gem home (~/.gem)
/// assert_eq!(
///     format_ruby_path("/home/user/.gem/ruby/3.3.0/gems/solargraph-0.50.0/lib/solargraph.rb"),
///     Some("(ruby 3.3.0 / solargraph-0.50.0) lib/solargraph.rb".into()),
/// );
///
/// // System Ruby stdlib (Debian)
/// assert_eq!(
///     format_ruby_path("/usr/lib/ruby/3.1.0/uri/common.rb"),
///     Some("(ruby 3.1.0) uri/common.rb".into()),
/// );
///
/// // macOS system Ruby stdlib
/// assert_eq!(
///     format_ruby_path("/System/Library/Frameworks/Ruby.framework/Versions/2.6/usr/lib/ruby/2.6.0/uri/common.rb"),
///     Some("(ruby 2.6) uri/common.rb".into()),
/// );
///
/// // Non-Ruby path returns None
/// assert_eq!(
///     format_ruby_path("/home/user/projects/myapp/lib/foo.rb"),
///     None,
/// );
/// ```
pub fn format_ruby_path(path: &str) -> Option<String> {
    // First try: extract ruby version from known installation root patterns.
    // Each pattern returns (ruby_version, rest_of_path_after_root).
    let ruby_root_patterns: &[(&str, fn(&str) -> Option<(&str, &str)>)] = &[
        // rbenv: ~/.rbenv/versions/3.3.0/...
        // frum:  ~/.frum/versions/3.3.0/...
        ("/versions/", |after| {
            let slash = after.find('/')?;
            let ver = &after[..slash];
            if looks_like_version(ver) {
                Some((ver, &after[slash + 1..]))
            } else {
                None
            }
        }),
        // mise: ~/.local/share/mise/installs/ruby/3.4.2/...
        // asdf: ~/.asdf/installs/ruby/3.3.0/...
        ("/installs/ruby/", |after| {
            let slash = after.find('/')?;
            let ver = &after[..slash];
            if looks_like_version(ver) {
                Some((ver, &after[slash + 1..]))
            } else {
                None
            }
        }),
        // rvm rubies: ~/.rvm/rubies/ruby-3.3.0/...
        // chruby/ruby-install: ~/.rubies/ruby-3.3.0/...
        //                      /opt/rubies/ruby-3.3.0/...
        ("rubies/ruby-", |after| {
            let slash = after.find('/')?;
            let ver = &after[..slash];
            if looks_like_version(ver) {
                Some((ver, &after[slash + 1..]))
            } else {
                None
            }
        }),
        // Homebrew Cellar: /opt/homebrew/Cellar/ruby/3.3.0/...
        //                  /usr/local/Cellar/ruby/3.3.0/...
        ("/Cellar/ruby/", |after| {
            let slash = after.find('/')?;
            let ver = &after[..slash];
            if looks_like_version(ver) {
                Some((ver, &after[slash + 1..]))
            } else {
                None
            }
        }),
        // macOS system Ruby: /System/Library/Frameworks/Ruby.framework/Versions/2.6/usr/...
        ("/Ruby.framework/Versions/", |after| {
            let slash = after.find('/')?;
            let ver = &after[..slash];
            if looks_like_version(ver) {
                Some((ver, &after[slash + 1..]))
            } else {
                None
            }
        }),
    ];

    for (marker, extractor) in ruby_root_patterns {
        if let Some(idx) = path.find(marker) {
            let after = &path[idx + marker.len()..];
            if let Some((ruby_ver, rest)) = extractor(after) {
                // Try gem sub-path within this root
                if let Some(result) = extract_gem_from_rest(rest, ruby_ver) {
                    return Some(result);
                }
                // Try stdlib sub-path: lib/ruby/<api_ver>/<file>
                if let Some(result) = extract_stdlib_from_rest(rest, ruby_ver) {
                    return Some(result);
                }
            }
        }
    }

    // Second try: gems that live outside the Ruby installation root.
    // These paths don't have a Ruby version root, but do have /gems/ structures.

    // RVM GEM_HOME: ~/.rvm/gems/ruby-3.3.0/gems/foo-1.0/...
    // RVM gemsets:  ~/.rvm/gems/ruby-3.3.0@mygemset/gems/foo-1.0/...
    if let Some(idx) = path.find("/.rvm/gems/ruby-") {
        let after = &path[idx + "/.rvm/gems/ruby-".len()..];
        return extract_rvm_gem_home(after);
    }

    // Homebrew shared gems: /opt/homebrew/lib/ruby/gems/3.3.0/gems/foo-1.0/...
    //                       /usr/local/lib/ruby/gems/3.3.0/gems/foo-1.0/...
    if let Some(result) = extract_standalone_gem_path(path, "/homebrew/lib/ruby/") {
        return Some(result);
    }
    if let Some(result) = extract_standalone_gem_path(path, "/usr/local/lib/ruby/") {
        return Some(result);
    }

    // Debian system gems: /var/lib/gems/3.1.0/gems/foo-1.0/...
    if let Some(idx) = path.find("/var/lib/gems/") {
        let after = &path[idx + "/var/lib/gems/".len()..];
        if let Some(slash) = after.find('/') {
            let ver = &after[..slash];
            if looks_like_version(ver) {
                let rest = &after[slash + 1..];
                if let Some(result) = extract_gem_folder(rest, ver) {
                    return Some(result);
                }
            }
        }
    }

    // User gem home: ~/.gem/ruby/3.3.0/gems/foo-1.0/... (chruby gem-home, --user-install)
    if let Some(idx) = path.find("/.gem/ruby/") {
        let after = &path[idx + "/.gem/ruby/".len()..];
        if let Some(slash) = after.find('/') {
            let ver = &after[..slash];
            if looks_like_version(ver) {
                let rest = &after[slash + 1..];
                if let Some(result) = extract_gem_folder(rest, ver) {
                    return Some(result);
                }
            }
        }
    }

    // System Ruby stdlib: /usr/lib/ruby/<ver>/... (Debian/Fedora)
    if let Some(idx) = path.find("/usr/lib/ruby/") {
        let after = &path[idx + "/usr/lib/ruby/".len()..];
        if let Some(slash) = after.find('/') {
            let ver = &after[..slash];
            if looks_like_version(ver) {
                let remainder = &after[slash + 1..];
                if !remainder.is_empty() && !remainder.starts_with("gems/") {
                    return Some(format!("(ruby {ver}) {remainder}"));
                }
            }
        }
    }

    None
}

/// Extracts gem info from a path relative to the Ruby installation root.
/// Looks for: lib/ruby/gems/<api_ver>/gems/<gem-name-ver>/<rest>
fn extract_gem_from_rest(rest: &str, ruby_ver: &str) -> Option<String> {
    // Find /gems/ followed by a version dir and another /gems/
    let marker = "lib/ruby/gems/";
    let idx = rest.find(marker)?;
    let after = &rest[idx + marker.len()..];
    // after = "<api_ver>/gems/<gem>/<rest>"
    let slash = after.find('/')?;
    let after_ver = &after[slash + 1..];
    extract_gem_folder(after_ver, ruby_ver)
}

/// Extracts stdlib info from a path relative to the Ruby installation root.
/// Looks for: lib/ruby/<api_ver>/<file> (but not lib/ruby/gems/)
fn extract_stdlib_from_rest(rest: &str, ruby_ver: &str) -> Option<String> {
    let marker = "lib/ruby/";
    let idx = rest.find(marker)?;
    let after = &rest[idx + marker.len()..];
    // Skip if this is a gems path
    if after.starts_with("gems/") {
        return None;
    }
    let slash = after.find('/')?;
    let ver_candidate = &after[..slash];
    if !looks_like_version(ver_candidate) {
        return None;
    }
    let remainder = &after[slash + 1..];
    if remainder.is_empty() {
        return None;
    }
    Some(format!("(ruby {ruby_ver}) {remainder}"))
}

/// RVM gem home: parses after "/.rvm/gems/ruby-"
/// Input: "3.3.0/gems/foo-1.0/lib/..." or "3.3.0@gemset/gems/foo-1.0/lib/..."
fn extract_rvm_gem_home(after: &str) -> Option<String> {
    // Find the version (may include @gemset)
    let slash = after.find('/')?;
    let ver_and_gemset = &after[..slash];
    // Extract just the version part (before @)
    let ruby_ver = ver_and_gemset
        .split('@')
        .next()
        .filter(|v| looks_like_version(v))?;
    let rest = &after[slash + 1..];
    extract_gem_folder(rest, ruby_ver)
}

/// Extracts gem from a standalone gem directory.
/// Looks for: <prefix>gems/<api_ver>/gems/<gem-name-ver>/<rest>
fn extract_standalone_gem_path(path: &str, prefix: &str) -> Option<String> {
    let idx = path.find(prefix)?;
    let after = &path[idx + prefix.len()..];
    // after should be "gems/<api_ver>/gems/<gem>/..." or "<api_ver>/..."
    let after = after.strip_prefix("gems/")?;
    let slash = after.find('/')?;
    let ver = &after[..slash];
    if !looks_like_version(ver) {
        return None;
    }
    let rest = &after[slash + 1..];
    extract_gem_folder(rest, ver)
}

/// Given a path starting after a version directory, looks for gems/<gem-name-ver>/<rest>.
fn extract_gem_folder(rest: &str, ruby_ver: &str) -> Option<String> {
    let after = rest.strip_prefix("gems/")?;
    let slash = after.find('/')?;
    let gem_name_ver = &after[..slash];
    let remainder = &after[slash + 1..];
    if remainder.is_empty() {
        return None;
    }
    Some(format!("(ruby {ruby_ver} / {gem_name_ver}) {remainder}"))
}

fn looks_like_version(s: &str) -> bool {
    !s.is_empty() && s.as_bytes()[0].is_ascii_digit()
}
