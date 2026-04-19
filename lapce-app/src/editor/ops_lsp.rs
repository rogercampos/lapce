//! LSP-driven navigation and code-action operations on [`EditorData`].
//!
//! Extracted from `editor.rs` as a separate `impl EditorData` block. Holds
//! the goto-definition flow (with a fallback to references when the symbol
//! is defined at the cursor itself), the code-action fetch pipeline, and
//! the popup trigger for showing available actions.

use std::collections::HashSet;

use floem::{
    ext_event::create_ext_action,
    reactive::{SignalGet, SignalUpdate, SignalWith},
};
use lapce_core::{
    buffer::rope_text::RopeText, language::LapceLanguage,
    rope_text_pos::RopeTextPosition,
};
use lapce_rpc::{plugin::PluginId, proxy::ProxyResponse};
use lsp_types::{CodeActionResponse, GotoDefinitionResponse, Location};

use crate::{
    command::InternalCommand,
    editor::{
        EditorData,
        location::{EditorLocation, EditorPosition},
        ruby::{dedup_ruby_stdlib_gems, ruby_filter_type_files, ruby_word_start},
    },
    lsp::path_from_url,
};

impl EditorData {
    /// Jump to the definition of the symbol at the cursor. If the symbol is
    /// defined at the cursor itself (meaning `textDocument/definition` returns
    /// the current position), fall back to `textDocument/references` so the
    /// user sees callers instead of a useless self-jump.
    pub(super) fn go_to_definition(&self) {
        let doc = self.doc();
        let path = match doc.loaded_file_path() {
            Some(path) => path,
            None => return,
        };

        let language = doc.syntax.with_untracked(|s| s.language);
        let offset = self.cursor().with_untracked(|c| c.offset());
        let (start_position, position) = doc.buffer.with_untracked(|buffer| {
            let mut start_offset = buffer.prev_code_boundary(offset);
            if language == LapceLanguage::Ruby {
                start_offset = ruby_word_start(buffer, start_offset);
            }
            let start_position = buffer.offset_to_position(start_offset);
            let position = buffer.offset_to_position(offset);
            (start_position, position)
        });

        enum DefinitionOrReferece {
            Location(EditorLocation),
            Locations(Vec<Location>),
        }

        let internal_command = self.common.internal_command;
        let cursor = self.cursor().read_only();
        let send = create_ext_action(self.scope, move |d| {
            let current_offset = cursor.with_untracked(|c| c.offset());
            if current_offset != offset {
                return;
            }

            match d {
                DefinitionOrReferece::Location(location) => {
                    internal_command
                        .send(InternalCommand::JumpToLocation { location });
                }
                DefinitionOrReferece::Locations(locations) => {
                    internal_command.send(InternalCommand::ShowDefinitionPicker {
                        offset,
                        locations,
                        language,
                    });
                }
            }
        });
        let proxy = self.common.proxy.clone();
        self.common.proxy.get_definition(
            offset,
            path.clone(),
            position,
            move |result| {
                if let Ok(ProxyResponse::GetDefinitionResponse {
                    definition, ..
                }) = result
                {
                    let mut all_locations: Vec<Location> = match definition {
                        GotoDefinitionResponse::Scalar(loc) => vec![loc],
                        GotoDefinitionResponse::Array(locs) => locs,
                        GotoDefinitionResponse::Link(links) => links
                            .into_iter()
                            .map(|link| Location {
                                uri: link.target_uri,
                                range: link.target_selection_range,
                            })
                            .collect(),
                    };
                    {
                        let mut seen = HashSet::new();
                        all_locations.retain(|l| {
                            seen.insert((l.uri.clone(), l.range.start.line))
                        });
                    }
                    if language == LapceLanguage::Ruby {
                        ruby_filter_type_files(&mut all_locations);
                        dedup_ruby_stdlib_gems(&mut all_locations);
                    }

                    if all_locations.is_empty() {
                        return;
                    }

                    // If single result at same position, fall back to references
                    if all_locations.len() == 1
                        && all_locations[0].range.start == start_position
                    {
                        proxy.get_references(
                            path.clone(),
                            position,
                            move |result| {
                                if let Ok(ProxyResponse::GetReferencesResponse {
                                    mut references,
                                }) = result
                                {
                                    {
                                        let mut seen = HashSet::new();
                                        references.retain(|l| {
                                            seen.insert((
                                                l.uri.clone(),
                                                l.range.start.line,
                                            ))
                                        });
                                    }
                                    if language == LapceLanguage::Ruby {
                                        ruby_filter_type_files(&mut references);
                                        dedup_ruby_stdlib_gems(&mut references);
                                    }
                                    if references.is_empty() {
                                        return;
                                    }
                                    if references.len() == 1 {
                                        let location = &references[0];
                                        send(DefinitionOrReferece::Location(
                                            EditorLocation {
                                                path: path_from_url(&location.uri),
                                                position: Some(
                                                    EditorPosition::Position(
                                                        location.range.start,
                                                    ),
                                                ),
                                                scroll_offset: None,
                                                same_editor_tab: false,
                                            },
                                        ));
                                    } else {
                                        send(DefinitionOrReferece::Locations(
                                            references,
                                        ));
                                    }
                                }
                            },
                        );
                    } else if all_locations.len() == 1 {
                        // Single result at different position — jump directly
                        let loc = &all_locations[0];
                        send(DefinitionOrReferece::Location(EditorLocation {
                            path: path_from_url(&loc.uri),
                            position: Some(EditorPosition::Position(
                                loc.range.start,
                            )),
                            scroll_offset: None,
                            same_editor_tab: false,
                        }));
                    } else {
                        // Multiple results — show picker
                        send(DefinitionOrReferece::Locations(all_locations));
                    }
                }
            },
        );
    }

    pub fn get_code_actions(&self) {
        let doc = self.doc();
        let path = match doc.loaded_file_path() {
            Some(path) => path,
            None => return,
        };

        let offset = self.cursor().with_untracked(|c| c.offset());
        let exists = doc
            .code_actions()
            .with_untracked(|c| c.contains_key(&offset));

        if exists {
            return;
        }

        // insert some empty data, so that we won't make the request again
        doc.code_actions().update(|c| {
            c.insert(offset, (PluginId(0), im::Vector::new()));
        });

        let (position, rev, diagnostics) = doc.buffer.with_untracked(|buffer| {
            let position = buffer.offset_to_position(offset);
            let rev = doc.rev();

            // Get the diagnostics for the current line, which the LSP might use to inform
            // what code actions are available (such as fixes for the diagnostics).
            let diagnostics = doc
                .diagnostics()
                .diagnostics_span
                .get_untracked()
                .iter_chunks(offset..offset)
                .filter(|(iv, _diag)| iv.start <= offset && iv.end >= offset)
                .map(|(_iv, diag)| diag)
                .cloned()
                .collect();

            (position, rev, diagnostics)
        });

        let send = create_ext_action(
            self.scope,
            move |resp: (PluginId, CodeActionResponse)| {
                if doc.rev() == rev {
                    doc.code_actions().update(|c| {
                        c.insert(offset, (resp.0, resp.1.into()));
                    });
                }
            },
        );

        self.common.proxy.get_code_actions(
            path,
            position,
            diagnostics,
            move |result| {
                if let Ok(ProxyResponse::GetCodeActionsResponse {
                    plugin_id,
                    resp,
                }) = result
                {
                    send((plugin_id, resp))
                }
            },
        );
    }

    pub fn show_code_actions(&self, mouse_click: bool) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let doc = self.doc();
        let code_actions = doc
            .code_actions()
            .with_untracked(|c| c.get(&offset).cloned());
        if let Some((plugin_id, code_actions)) = code_actions {
            if !code_actions.is_empty() {
                self.common.internal_command.send(
                    InternalCommand::ShowCodeActions {
                        offset,
                        mouse_click,
                        plugin_id,
                        code_actions,
                    },
                );
            }
        }
    }
}
