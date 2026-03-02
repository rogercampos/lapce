use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    fmt::Write,
    path::Path,
    str::FromStr,
};

use lapce_rpc::style::{LineStyle, Style};
use once_cell::sync::Lazy;
use regex::Regex;
use strum_macros::{AsRefStr, Display, EnumMessage, EnumString, IntoStaticStr};
use tracing::{Level, event};
use tree_sitter::{Point, TreeCursor};

use crate::{
    directory::Directory,
    syntax::highlight::{HighlightConfiguration, HighlightIssue},
};

/// Represents the indentation style for a language.
/// Uses const fn to return static string slices, avoiding runtime allocation.
#[remain::sorted]
pub enum Indent {
    Space(u8),
    Tab,
}

impl Indent {
    const fn tab() -> &'static str {
        Indent::Tab.as_str()
    }

    const fn space(count: u8) -> &'static str {
        Indent::Space(count).as_str()
    }

    /// Returns the indent string as a static slice. Only supports 2, 4, and 8
    /// space widths because const fn cannot do dynamic string generation.
    /// Any unsupported space count falls through to 8 spaces.
    const fn as_str(&self) -> &'static str {
        match self {
            Indent::Tab => "\u{0009}",
            #[allow(clippy::wildcard_in_or_patterns)]
            Indent::Space(v) => match v {
                2 => "\u{0020}\u{0020}",
                4 => "\u{0020}\u{0020}\u{0020}\u{0020}",
                8 | _ => {
                    "\u{0020}\u{0020}\u{0020}\u{0020}\u{0020}\u{0020}\u{0020}\u{0020}"
                }
            },
        }
    }
}

/// Default node types to traverse when building the code glance (minimap-like overview).
/// "source_file" is always the root node in tree-sitter ASTs, so traversing it
/// means we walk the entire top-level structure.
const DEFAULT_CODE_GLANCE_LIST: &[&str] = &["source_file"];
/// "source_file" is ignored in the glance display itself since it spans the
/// entire file -- we only want its children's start/end lines.
const DEFAULT_CODE_GLANCE_IGNORE_LIST: &[&str] = &["source_file"];

#[macro_export]
macro_rules! comment_properties {
    () => {
        CommentProperties {
            single_line_start: None,
            single_line_end: None,

            multi_line_start: None,
            multi_line_end: None,
            multi_line_prefix: None,
        }
    };
    ($s:expr) => {
        CommentProperties {
            single_line_start: Some($s),
            single_line_end: None,

            multi_line_start: None,
            multi_line_end: None,
            multi_line_prefix: None,
        }
    };
    ($s:expr, $e:expr) => {
        CommentProperties {
            single_line_start: Some($s),
            single_line_end: Some($e),

            multi_line_start: None,
            multi_line_end: None,
            multi_line_prefix: None,
        }
    };
    ($sl_s:expr, $sl_e:expr, $ml_s:expr, $ml_e:expr) => {
        CommentProperties {
            single_line_start: Some($sl_s),
            single_line_end: Some($sl_e),

            multi_line_start: Some($ml_s),
            multi_line_end: Some($ml_e),
            multi_line_prefix: None,
        }
    };
}

#[derive(Eq, PartialEq, Hash, Clone, Copy, Debug, PartialOrd, Ord, Default)]
pub struct SyntaxProperties {
    /// An extra check to make sure that the array elements are in the correct order.
    /// If this id does not match the enum value, a panic will happen with a debug assertion message.
    id: LapceLanguage,

    /// All tokens that can be used for comments in language
    comment: CommentProperties,
    /// The indent unit.
    /// "  " for bash, "    " for rust, for example.
    indent: &'static str,
    /// Filenames that belong to this language
    /// `["Dockerfile"]` for Dockerfile, `[".editorconfig"]` for EditorConfig
    files: &'static [&'static str],
    /// File name extensions to determine the language.
    /// `["py"]` for python, `["rs"]` for rust, for example.
    extensions: &'static [&'static str],
    /// Tree-sitter properties
    tree_sitter: TreeSitterProperties,
}

#[derive(Eq, PartialEq, Hash, Clone, Copy, Debug, PartialOrd, Ord, Default)]
struct TreeSitterProperties {
    /// the grammar name that's in the grammars folder
    grammar: Option<&'static str>,
    /// the grammar fn name
    grammar_fn: Option<&'static str>,
    /// the query folder name
    query: Option<&'static str>,
    /// Preface: Originally this feature was called "Code Lens", which is not
    /// an LSP "Code Lens". It is renamed to "Code Glance", below doc text is
    /// left unchanged.
    ///
    /// Lists of tree-sitter node types that control how code lenses are built.
    /// The first is a list of nodes that should be traversed and included in
    /// the lens, along with their children. The second is a list of nodes that
    /// should be excluded from the lens, though they will still be traversed.
    /// See `walk_tree` for more details.
    ///
    /// The tree-sitter playground may be useful when creating these lists:
    /// https://tree-sitter.github.io/tree-sitter/playground
    ///
    /// If unsure, use `DEFAULT_CODE_GLANCE_LIST` and
    /// `DEFAULT_CODE_GLANCE_IGNORE_LIST`.
    code_glance: (&'static [&'static str], &'static [&'static str]),
    /// the tree-sitter tag names that can be put in sticky headers
    sticky_headers: &'static [&'static str],
}

impl TreeSitterProperties {
    const DEFAULT: Self = Self {
        grammar: None,
        grammar_fn: None,
        query: None,
        code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
        sticky_headers: &[],
    };
}

#[derive(Eq, PartialEq, Hash, Clone, Copy, Debug, PartialOrd, Ord, Default)]
struct CommentProperties {
    /// Single line comment token used when commenting out one line.
    /// "#" for python, "//" for rust for example.
    single_line_start: Option<&'static str>,
    single_line_end: Option<&'static str>,

    /// Multi line comment token used when commenting a selection of lines.
    /// "#" for python, "//" for rust for example.
    multi_line_start: Option<&'static str>,
    multi_line_end: Option<&'static str>,
    multi_line_prefix: Option<&'static str>,
}

/// NOTE: Keep the enum variants "fieldless" so they can cast to usize as array
/// indices into the LANGUAGES array.  See method `LapceLanguage::properties`.
///
/// Do not assign values to the variants because the number of variants and
/// number of elements in the LANGUAGES array change as different features
/// selected by the cargo build command.
///
/// The `#[remain::sorted]` attribute enforces alphabetical ordering of variants
/// at compile time, which also determines the array index order in LANGUAGES.
#[derive(
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    Clone,
    Copy,
    Debug,
    Display,
    AsRefStr,
    IntoStaticStr,
    EnumString,
    EnumMessage,
    Default,
)]
#[strum(ascii_case_insensitive)]
#[remain::sorted]
pub enum LapceLanguage {
    // Do not move
    #[remain::unsorted]
    #[default]
    #[strum(message = "Plain Text")]
    PlainText,

    #[strum(message = "Bash")]
    Bash,
    #[strum(message = "C")]
    C,
    #[strum(message = "Clojure")]
    Clojure,
    #[strum(message = "CMake")]
    Cmake,
    Comment,
    #[strum(message = "C++")]
    Cpp,
    #[strum(message = "C#")]
    Csharp,
    #[strum(message = "CSS")]
    Css,
    #[strum(message = "D")]
    D,
    #[strum(message = "Dart")]
    Dart,
    #[strum(message = "Diff")]
    Diff,
    #[strum(message = "Dockerfile")]
    Dockerfile,
    #[strum(message = "Elixir")]
    Elixir,
    #[strum(message = "Elm")]
    Elm,
    #[strum(message = "Erlang")]
    Erlang,
    #[strum(message = "Fish Shell")]
    Fish,
    #[strum(message = "Git (config)")]
    GitConfig,
    #[strum(message = "Glimmer")]
    Glimmer,
    #[strum(message = "GLSL")]
    Glsl,
    #[strum(message = "Go")]
    Go,
    #[strum(message = "Hare")]
    Hare,
    #[strum(message = "Haskell")]
    Haskell,
    #[strum(message = "Haxe")]
    Haxe,
    #[strum(message = "HCL")]
    Hcl,
    #[strum(message = "HTML")]
    Html,
    #[strum(message = "Java")]
    Java,
    #[strum(message = "JavaScript")]
    Javascript,
    #[strum(message = "JSON")]
    Json,
    #[strum(message = "JavaScript React")]
    Jsx,
    #[strum(message = "Julia")]
    Julia,
    #[strum(message = "Just")]
    Just,
    #[strum(message = "Kotlin")]
    Kotlin,
    #[strum(message = "Kotlin Build Script")]
    KotlinBuildScript,
    #[strum(message = "LaTeX")]
    Latex,
    #[strum(message = "Log")]
    Log,
    #[strum(message = "Lua")]
    Lua,
    #[strum(message = "Markdown")]
    Markdown,
    #[strum(serialize = "markdown.inline")]
    MarkdownInline,
    #[strum(message = "Nix")]
    Nix,
    #[strum(message = "Ocaml")]
    Ocaml,
    #[strum(serialize = "ocaml.interface")]
    OcamlInterface,
    #[strum(message = "PHP")]
    Php,
    #[strum(message = "PowerShell")]
    PowerShell,
    #[strum(message = "Prisma")]
    Prisma,
    #[strum(message = "Proto")]
    ProtoBuf,
    #[strum(message = "Python")]
    Python,
    #[strum(message = "QL")]
    Ql,
    #[strum(message = "R")]
    R,
    #[strum(message = "Ruby")]
    Ruby,
    #[strum(message = "Rust")]
    Rust,
    #[strum(message = "Scheme")]
    Scheme,
    #[strum(message = "SCSS")]
    Scss,
    #[strum(message = "Shell Script (POSIX)")]
    ShellScript,
    #[strum(message = "SQL")]
    Sql,
    #[strum(message = "Svelte")]
    Svelte,
    #[strum(message = "Swift")]
    Swift,
    #[strum(message = "TCL")]
    Tcl,
    #[strum(message = "TOML")]
    Toml,
    #[strum(message = "Tsx")]
    Tsx,
    #[strum(message = "TypeScript")]
    Typescript,
    #[strum(message = "Vue")]
    Vue,
    #[strum(message = "WASM")]
    Wasm,
    #[strum(message = "WGSL")]
    Wgsl,
    #[strum(message = "WIT")]
    Wit,
    #[strum(message = "XML")]
    Xml,
    #[strum(message = "YAML")]
    Yaml,
    #[strum(message = "Zig")]
    Zig,
}

/// NOTE: Elements in the array must be in the same order as the enum variants of
/// `LapceLanguage` as they will be accessed using the enum variants as indices.
const LANGUAGES: &[SyntaxProperties] = &[
    // Undetected/unmatched fallback or just plain file
    SyntaxProperties {
        id: LapceLanguage::PlainText,
        indent: Indent::tab(),
        files: &[],
        extensions: &["txt"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    // Languages
    SyntaxProperties {
        id: LapceLanguage::Bash,
        indent: Indent::space(2),
        files: &[],
        extensions: &["bash", "sh"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::C,
        indent: Indent::space(4),
        files: &[],
        extensions: &["c", "h"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
            sticky_headers: &["function_definition", "struct_specifier"],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Clojure,
        indent: Indent::space(2),
        files: &[],
        extensions: &["clj", "edn", "cljs", "cljc", "cljd", "bb", "clj_kondo"],
        comment: comment_properties!(";"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Cmake,
        indent: Indent::space(2),
        files: &["cmakelists"],
        extensions: &["cmake"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
            sticky_headers: &["function_definition"],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Comment,
        indent: Indent::space(2),
        files: &[],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Cpp,
        indent: Indent::space(4),
        files: &[],
        extensions: &["cpp", "cxx", "cc", "c++", "hpp", "hxx", "hh", "h++"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
            sticky_headers: &[
                "function_definition",
                "class_specifier",
                "struct_specifier",
            ],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Csharp,
        indent: Indent::space(2),
        files: &[],
        extensions: &["cs", "csx"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
            sticky_headers: &[
                "interface_declaration",
                "class_declaration",
                "enum_declaration",
                "struct_declaration",
                "record_declaration",
                "record_struct_declaration",
                "namespace_declaration",
                "constructor_declaration",
                "destructor_declaration",
                "method_declaration",
            ],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Css,
        indent: Indent::space(2),
        files: &[],
        extensions: &["css"],
        comment: comment_properties!("/*", "*/"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::D,
        indent: Indent::space(4),
        files: &[],
        extensions: &["d", "di", "dlang"],
        comment: CommentProperties {
            single_line_start: Some("//"),
            single_line_end: None,
            multi_line_start: Some("/+"),
            multi_line_prefix: None,
            multi_line_end: Some("+/"),
        },
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Dart,
        indent: Indent::space(2),
        files: &[],
        extensions: &["dart"],
        comment: CommentProperties {
            single_line_start: Some("//"),
            single_line_end: None,

            multi_line_start: Some("/*"),
            multi_line_prefix: None,
            multi_line_end: Some("*/"),
        },
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (
                &["program", "class_definition"],
                &[
                    "program",
                    "import_or_export",
                    "comment",
                    "documentation_comment",
                ],
            ),
            sticky_headers: &["class_definition"],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Diff,
        indent: Indent::tab(),
        files: &[],
        extensions: &["diff", "patch"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Dockerfile,
        indent: Indent::space(2),
        files: &["Dockerfile", "Containerfile"],
        extensions: &["containerfile", "dockerfile"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Elixir,
        indent: Indent::space(2),
        files: &[],
        extensions: &["ex", "exs", "eex", "heex", "sface"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
            sticky_headers: &["do_block"],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Elm,
        indent: Indent::space(4),
        files: &[],
        extensions: &["elm"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Erlang,
        indent: Indent::space(4),
        files: &[],
        extensions: &["erl", "hrl"],
        comment: comment_properties!("%"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Fish,
        indent: Indent::tab(),
        files: &[],
        extensions: &["fish"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::GitConfig,
        indent: Indent::tab(),
        files: &[".gitconfig", ".git/config"],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Glimmer,
        indent: Indent::space(2),
        files: &[],
        extensions: &["hbs"],
        comment: comment_properties!("{{!", "!}}"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Glsl,
        indent: Indent::space(2),
        files: &[],
        extensions: &[
            "glsl", "cs", "vs", "gs", "fs", "csh", "vsh", "gsh", "fsh", "cshader",
            "vshader", "gshader", "fshader", "comp", "vert", "geom", "frag", "tesc",
            "tese", "mesh", "task", "rgen", "rint", "rahit", "rchit", "rmiss",
            "rcall",
        ],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Go,
        indent: Indent::tab(),
        files: &[],
        extensions: &["go"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (
                &[
                    "source_file",
                    "type_declaration",
                    "type_spec",
                    "interface_type",
                    "method_spec_list",
                ],
                &["source_file", "comment", "line_comment"],
            ),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Hare,
        indent: Indent::space(8),
        files: &[],
        extensions: &["ha"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Haskell,
        indent: Indent::space(2),
        files: &[],
        extensions: &["hs"],
        comment: comment_properties!("--"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Haxe,
        indent: Indent::space(2),
        files: &[],
        extensions: &["hx"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Hcl,
        indent: Indent::space(2),
        files: &[],
        extensions: &["hcl", "tf"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Html,
        indent: Indent::space(4),
        files: &[],
        extensions: &["html", "htm"],
        comment: comment_properties!("<!--", "-->"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Java,
        indent: Indent::space(4),
        files: &[],
        extensions: &["java"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Javascript,
        indent: Indent::space(2),
        files: &[],
        extensions: &["js", "cjs", "mjs"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (&["source_file", "program"], &["source_file"]),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Json,
        indent: Indent::space(4),
        files: &[],
        extensions: &["json", "har"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Jsx,
        indent: Indent::space(2),
        files: &[],
        extensions: &["jsx"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: Some("javascript"),
            grammar_fn: Some("javascript"),
            query: Some("jsx"),
            code_glance: (&["source_file", "program"], &["source_file"]),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Julia,
        indent: Indent::space(4),
        files: &[],
        extensions: &["julia", "jl"],
        comment: CommentProperties {
            single_line_start: Some("#"),
            single_line_end: None,
            multi_line_start: Some("#="),
            multi_line_prefix: None,
            multi_line_end: Some("=#"),
        },
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Just,
        indent: Indent::tab(),
        files: &["justfile", "Justfile", ".justfile", ".Justfile"],
        extensions: &["just"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Kotlin,
        indent: Indent::space(2),
        files: &[],
        extensions: &["kt"],
        comment: CommentProperties {
            single_line_start: Some("//"),
            single_line_end: None,

            multi_line_start: Some("/*"),
            multi_line_prefix: None,
            multi_line_end: Some("*/"),
        },
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::KotlinBuildScript,
        indent: Indent::space(2),
        files: &[],
        extensions: &["kts"],
        comment: CommentProperties {
            single_line_start: Some("//"),
            single_line_end: None,

            multi_line_start: Some("/*"),
            multi_line_prefix: None,
            multi_line_end: Some("*/"),
        },
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Latex,
        indent: Indent::space(2),
        files: &[],
        extensions: &["tex"],
        comment: comment_properties!("%"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Log,
        indent: Indent::tab(),
        files: &["log.txt"],
        extensions: &["log"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Lua,
        indent: Indent::space(2),
        files: &[],
        extensions: &["lua"],
        comment: comment_properties!("--"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Markdown,
        indent: Indent::space(4),
        files: &[],
        extensions: &["md"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::MarkdownInline,
        indent: Indent::space(4),
        // markdown inline is only used as an injection by the Markdown language
        files: &[],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties {
            grammar: Some("markdown_inline"),
            grammar_fn: Some("markdown_inline"),
            query: Some("markdown.inline"),
            code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Nix,
        indent: Indent::space(2),
        files: &[],
        extensions: &["nix"],
        comment: CommentProperties {
            single_line_start: Some("#"),
            single_line_end: None,

            multi_line_start: Some("/*"),
            multi_line_prefix: None,
            multi_line_end: Some("*/"),
        },
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Ocaml,
        indent: Indent::space(2),
        files: &[],
        extensions: &["ml"],
        comment: CommentProperties {
            single_line_start: Some("(*"),
            single_line_end: Some("*)"),

            multi_line_start: Some("(*"),
            multi_line_prefix: Some("*"),
            multi_line_end: Some("*)"),
        },
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::OcamlInterface,
        indent: Indent::space(2),
        files: &[],
        extensions: &["mli"],
        comment: comment_properties!("(*"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Php,
        indent: Indent::space(2),
        files: &[],
        extensions: &["php"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (
                &[
                    "program",
                    "class_declaration",
                    "trait_declaration",
                    "interface_declaration",
                    "declaration_list",
                    "method_declaration",
                    "function_declaration",
                ],
                &[
                    "program",
                    "php_tag",
                    "comment",
                    "namespace_definition",
                    "namespace_use_declaration",
                    "use_declaration",
                    "const_declaration",
                    "property_declaration",
                    "expression_statement",
                ],
            ),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::PowerShell,
        indent: Indent::space(4),
        files: &[],
        extensions: &["ps1", "psm1", "psd1", "ps1xml"],
        comment: CommentProperties {
            single_line_start: Some("#"),
            single_line_end: None,
            multi_line_start: Some("<#"),
            multi_line_end: Some("#>"),
            multi_line_prefix: None,
        },
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Prisma,
        indent: Indent::space(4),
        files: &[],
        extensions: &["prisma"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::ProtoBuf,
        indent: Indent::space(2),
        files: &[],
        extensions: &["proto"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Python,
        indent: Indent::space(4),
        files: &[],
        extensions: &["py", "pyi", "pyc", "pyd", "pyw"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (
                &[
                    "source_file",
                    "module",
                    "class_definition",
                    "class",
                    "identifier",
                    "decorated_definition",
                    "block",
                ],
                &["source_file", "import_statement", "import_from_statement"],
            ),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Ql,
        indent: Indent::space(2),
        files: &[],
        extensions: &["ql"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::R,
        indent: Indent::space(2),
        files: &[],
        extensions: &["r"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Ruby,
        indent: Indent::space(2),
        files: &[],
        extensions: &["rb"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
            sticky_headers: &["module", "class", "method", "do_block"],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Rust,
        indent: Indent::space(4),
        files: &[],
        extensions: &["rs"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (
                &["source_file", "impl_item", "trait_item", "declaration_list"],
                &["source_file", "use_declaration", "line_comment"],
            ),
            sticky_headers: &[
                "struct_item",
                "enum_item",
                "function_item",
                "impl_item",
            ],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Scheme,
        indent: Indent::space(2),
        files: &[],
        extensions: &["scm", "ss"],
        comment: comment_properties!(";"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Scss,
        indent: Indent::space(2),
        files: &[],
        extensions: &["scss"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::ShellScript,
        indent: Indent::space(2),
        files: &[],
        extensions: &["sh"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Sql,
        indent: Indent::space(2),
        files: &[],
        extensions: &["sql"],
        comment: comment_properties!("--"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Svelte,
        indent: Indent::space(2),
        files: &[],
        extensions: &["svelte"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Swift,
        indent: Indent::space(2),
        files: &[],
        extensions: &["swift"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Tcl,
        indent: Indent::tab(),
        files: &[],
        extensions: &["tcl"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Toml,
        indent: Indent::space(2),
        files: &["Cargo.lock"],
        extensions: &["toml"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Tsx,
        indent: Indent::space(4),
        files: &[],
        extensions: &["tsx"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: Some("tsx"),
            grammar_fn: Some("tsx"),
            query: Some("tsx"),
            code_glance: (&["source_file", "program"], &["source_file"]),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Typescript,
        indent: Indent::space(4),
        files: &[],
        extensions: &["ts", "cts", "mts"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: Some("typescript"),
            grammar_fn: Some("typescript"),
            query: Some("typescript"),
            code_glance: (&["source_file", "program"], &["source_file"]),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: LapceLanguage::Vue,
        indent: Indent::space(2),
        files: &[],
        extensions: &["vue"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Wasm,
        indent: Indent::space(4),
        files: &[],
        extensions: &["wasm"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Wgsl,
        indent: Indent::space(4),
        files: &[],
        extensions: &["wgsl"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Wit,
        indent: Indent::space(4),
        files: &[],
        extensions: &["wit"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Xml,
        indent: Indent::space(4),
        files: &[],
        extensions: &["xml", "csproj"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Yaml,
        indent: Indent::space(2),
        files: &[],
        extensions: &["yml", "yaml"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: LapceLanguage::Zig,
        indent: Indent::space(4),
        files: &[],
        extensions: &["zig"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
];

impl LapceLanguage {
    const HIGHLIGHTS_INJECTIONS_FILE_NAME: &'static str = "injections.scm";
    const HIGHLIGHTS_QUERIES_FILE_NAME: &'static str = "highlights.scm";

    pub fn from_path(path: &Path) -> LapceLanguage {
        Self::from_path_raw(path).unwrap_or(LapceLanguage::PlainText)
    }

    pub fn from_path_raw(path: &Path) -> Option<LapceLanguage> {
        let filename = path.file_name().and_then(|s| s.to_str());
        let extension = path
            .extension()
            .and_then(|s| s.to_str().map(|s| s.to_lowercase()));
        // NOTE: This is a linear search.  It is assumed that this function
        // isn't called in any tight loop.
        for properties in LANGUAGES {
            if properties.files.iter().any(|f| Some(*f) == filename) {
                return Some(properties.id);
            }
            if properties
                .extensions
                .iter()
                .any(|e| Some(*e) == extension.as_deref())
            {
                return Some(properties.id);
            }
        }

        None
    }

    pub fn from_name(name: &str) -> Option<LapceLanguage> {
        match LapceLanguage::from_str(name.to_lowercase().as_str()) {
            Ok(v) => Some(v),
            Err(e) => {
                event!(Level::DEBUG, "failed parsing `{name}` LapceLanguage: {e}");
                None
            }
        }
    }

    pub fn languages() -> Vec<&'static str> {
        let mut langs = vec![];
        for l in LANGUAGES {
            // Get only languages with display name to hide inline grammars
            if let Some(lang) = strum::EnumMessage::get_message(&l.id) {
                langs.push(lang)
            }
        }
        langs
    }

    // NOTE: Instead of using `&LANGUAGES[*self as usize]` directly, the
    // `debug_assertion` gives better feedback should something has gone wrong
    // badly.
    fn properties(&self) -> &SyntaxProperties {
        let i = *self as usize;
        let l = &LANGUAGES[i];
        assert!(
            l.id == *self,
            "LANGUAGES[{i}]: Setting::id mismatch: {:?} != {:?}",
            l.id,
            self
        );
        l
    }

    pub fn name(&self) -> &'static str {
        strum::EnumMessage::get_message(self).unwrap_or(self.into())
    }

    pub fn sticky_header_tags(&self) -> &[&'static str] {
        self.properties().tree_sitter.sticky_headers
    }

    pub fn comment_token(&self) -> &'static str {
        self.properties()
            .comment
            .single_line_start
            .unwrap_or_default()
    }

    pub fn indent_unit(&self) -> &str {
        self.properties().indent
    }

    fn get_grammar(&self) -> Option<tree_sitter::Language> {
        let grammar_name = self.grammar_name();
        let grammar_fn_name = self.grammar_fn_name();

        if let Some(grammars_dir) = Directory::grammars_directory() {
            match self::load_grammar(&grammar_name, &grammar_fn_name, &grammars_dir)
            {
                Ok(grammar) => {
                    return Some(grammar);
                }
                Err(err) => {
                    if self != &LapceLanguage::PlainText {
                        tracing::error!("{:?} {:?}", self, err);
                    }
                }
            }
        };

        None
    }

    fn query_name(&self) -> String {
        self.properties()
            .tree_sitter
            .query
            .unwrap_or(self.properties().id.as_ref())
            .to_lowercase()
    }

    fn grammar_name(&self) -> String {
        self.properties()
            .tree_sitter
            .grammar
            .unwrap_or(self.properties().id.as_ref())
            .to_lowercase()
    }

    fn grammar_fn_name(&self) -> String {
        self.properties()
            .tree_sitter
            .grammar_fn
            .unwrap_or(self.properties().id.as_ref())
            .to_lowercase()
    }

    fn get_grammar_query(&self) -> (String, String) {
        let query_name = self.query_name();

        // Try reading highlights from user config dir
        if let Some(queries_dir) = Directory::queries_directory() {
            return (
                read_grammar_query(
                    &queries_dir,
                    &query_name,
                    Self::HIGHLIGHTS_QUERIES_FILE_NAME,
                ),
                read_grammar_query(
                    &queries_dir,
                    &query_name,
                    Self::HIGHLIGHTS_INJECTIONS_FILE_NAME,
                ),
            );
        }

        ("".to_string(), "".to_string())
    }

    pub(crate) fn new_highlight_config(
        &self,
    ) -> Result<HighlightConfiguration, HighlightIssue> {
        let grammar = self.get_grammar().ok_or(HighlightIssue::NotAvailable)?;
        let (query, injection) = self.get_grammar_query();

        match HighlightConfiguration::new(grammar, &query, &injection, "") {
            Ok(x) => Ok(x),
            Err(x) => {
                let str = format!(
                    "Encountered {x:?} while trying to construct HighlightConfiguration for {}",
                    self.name()
                );
                event!(Level::ERROR, "{str}");
                Err(HighlightIssue::Error(str))
            }
        }
    }

    pub(crate) fn walk_tree(
        &self,
        cursor: &mut TreeCursor,
        normal_lines: &mut HashSet<usize>,
    ) {
        let (list, ignore_list) = self.properties().tree_sitter.code_glance;
        walk_tree(cursor, normal_lines, list, ignore_list);
    }
}

/// Dynamically loads a tree-sitter grammar from a shared library (.dylib/.so/.dll).
/// Tries the "lib"-prefixed path first, then falls back to the non-prefixed path
/// for backward compatibility with older grammar installations.
///
/// SAFETY: Uses `libloading` to call into C ABI functions. The library is
/// intentionally leaked via `mem::forget` to keep the grammar function pointers
/// valid for the lifetime of the process.
fn load_grammar(
    grammar_name: &str,
    grammar_fn_name: &str,
    path: &Path,
) -> Result<tree_sitter::Language, HighlightIssue> {
    let mut library_path = path.join(format!("libtree-sitter-{grammar_name}"));
    library_path.set_extension(std::env::consts::DLL_EXTENSION);

    if !library_path.exists() {
        event!(Level::WARN, "Grammar not found at: {library_path:?}");

        // Load backwar compat libraries
        library_path = path.join(format!("tree-sitter-{grammar_name}"));
        library_path.set_extension(std::env::consts::DLL_EXTENSION);

        if !library_path.exists() {
            event!(Level::WARN, "Grammar not found at: {library_path:?}");
            return Err(HighlightIssue::Error("grammar not found".to_string()));
        }
    }

    event!(Level::DEBUG, "Loading grammar from user grammar dir");
    let library = match unsafe { libloading::Library::new(&library_path) } {
        Ok(v) => v,
        Err(e) => {
            let err = format!("Failed to load '{}': '{e}'", library_path.display());
            event!(Level::ERROR, err);
            return Err(HighlightIssue::Error(err));
        }
    };

    let language_fn_name =
        format!("tree_sitter_{}", grammar_fn_name.replace('-', "_"));
    event!(
        Level::DEBUG,
        "Loading grammar with address: '{language_fn_name}'"
    );
    let language = unsafe {
        let language_fn: libloading::Symbol<
            unsafe extern "C" fn() -> tree_sitter::Language,
        > = match library.get(language_fn_name.as_bytes()) {
            Ok(v) => v,
            Err(e) => {
                let err = format!("Failed to load '{language_fn_name}': '{e}'");
                event!(Level::ERROR, err);
                if let Some(e) = library.close().err() {
                    event!(Level::ERROR, "Failed to drop loaded library: {e}");
                };
                return Err(HighlightIssue::Error(err));
            }
        };
        language_fn()
    };
    // Intentionally leak the library handle so the grammar's function pointers
    // remain valid for the entire process lifetime. Dropping the library would
    // unload the .so/.dylib and invalidate the Language object.
    std::mem::forget(library);

    Ok(language)
}

/// Walk an AST and determine which lines to include in the code glance.
///
/// Node types listed in `list` will be walked, along with their children. All
/// nodes encountered will be included, unless they are listed in `ignore_list`.
fn walk_tree(
    cursor: &mut TreeCursor,
    normal_lines: &mut HashSet<usize>,
    list: &[&str],
    ignore_list: &[&str],
) {
    let node = cursor.node();
    let start_pos = node.start_position();
    let end_pos = node.end_position();
    let kind = node.kind().trim();
    if !ignore_list.contains(&kind) && !kind.is_empty() {
        normal_lines.insert(start_pos.row);
        normal_lines.insert(end_pos.row);
    }

    if list.contains(&kind) && cursor.goto_first_child() {
        loop {
            walk_tree(cursor, normal_lines, list, ignore_list);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn add_bracket_pos(
    bracket_pos: &mut HashMap<usize, Vec<LineStyle>>,
    start_pos: Point,
    color: &str,
) {
    let line_style = LineStyle {
        start: start_pos.column,
        end: start_pos.column + 1,
        style: Style {
            fg_color: Some(color.to_string()),
        },
    };
    match bracket_pos.entry(start_pos.row) {
        Entry::Vacant(v) => _ = v.insert(vec![line_style]),
        Entry::Occupied(mut o) => o.get_mut().push(line_style),
    }
}

/// Walks the tree-sitter AST to find bracket characters and assign them
/// rainbow-colored styles based on nesting depth. Uses `overflowing_sub`
/// to gracefully handle unmatched closing brackets (marking them as "unpaired").
pub(crate) fn walk_tree_bracket_ast(
    cursor: &mut TreeCursor,
    level: &mut usize,
    counter: &mut usize,
    bracket_pos: &mut HashMap<usize, Vec<LineStyle>>,
    palette: &[&str],
) {
    if cursor.node().kind().ends_with('(')
        || cursor.node().kind().ends_with('{')
        || cursor.node().kind().ends_with('[')
    {
        let row = cursor.node().end_position().row;
        let col = cursor.node().end_position().column - 1;
        let start_pos = Point::new(row, col);
        add_bracket_pos(bracket_pos, start_pos, palette[*level % palette.len()]);
        *level += 1;
    } else if cursor.node().kind().ends_with(')')
        || cursor.node().kind().ends_with('}')
        || cursor.node().kind().ends_with(']')
    {
        let (new_level, overflow) = (*level).overflowing_sub(1);
        let row = cursor.node().end_position().row;
        let col = cursor.node().end_position().column - 1;
        let start_pos = Point::new(row, col);
        if overflow {
            add_bracket_pos(bracket_pos, start_pos, "bracket.unpaired");
        } else {
            *level = new_level;
            add_bracket_pos(bracket_pos, start_pos, palette[*level % palette.len()]);
        }
    }
    *counter += 1;
    if cursor.goto_first_child() {
        loop {
            walk_tree_bracket_ast(cursor, level, counter, bracket_pos, palette);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Reads a tree-sitter query file from disk, recursively resolving
/// `;; inherits: <language>` directives by inlining the referenced
/// query files. This mirrors the convention used by Helix/Neovim where
/// query files can inherit from other languages' queries.
fn read_grammar_query(queries_dir: &Path, name: &str, kind: &str) -> String {
    static INHERITS_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r";+\s*inherits\s*:?\s*([a-z_,()-]+)\s*").unwrap());

    let file = queries_dir.join(name).join(kind);
    let query = std::fs::read_to_string(&file).unwrap_or_else(|err| {
        tracing::event!(
            tracing::Level::WARN,
            "Failed to read queries at: {file:?}, {err}"
        );
        String::new()
    });

    INHERITS_REGEX
        .replace_all(&query, |captures: &regex::Captures| {
            captures[1]
                .split(',')
                .fold(String::new(), |mut output, name| {
                    write!(
                        output,
                        "\n{}\n",
                        read_grammar_query(queries_dir, name, kind)
                    )
                    .unwrap();
                    output
                })
        })
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::LapceLanguage;

    #[test]
    fn test_lanaguage_from_path() {
        let l = LapceLanguage::from_path(&PathBuf::new().join("test.rs"));
        assert_eq!(l, LapceLanguage::Rust);
    }

    // --- from_path with various extensions ---

    #[test]
    fn from_path_python() {
        assert_eq!(
            LapceLanguage::from_path(&PathBuf::from("script.py")),
            LapceLanguage::Python,
        );
    }

    #[test]
    fn from_path_javascript_variants() {
        assert_eq!(
            LapceLanguage::from_path(&PathBuf::from("app.js")),
            LapceLanguage::Javascript,
        );
        assert_eq!(
            LapceLanguage::from_path(&PathBuf::from("app.mjs")),
            LapceLanguage::Javascript,
        );
        assert_eq!(
            LapceLanguage::from_path(&PathBuf::from("app.cjs")),
            LapceLanguage::Javascript,
        );
    }

    #[test]
    fn from_path_typescript() {
        assert_eq!(
            LapceLanguage::from_path(&PathBuf::from("index.ts")),
            LapceLanguage::Typescript,
        );
        assert_eq!(
            LapceLanguage::from_path(&PathBuf::from("index.tsx")),
            LapceLanguage::Tsx,
        );
    }

    #[test]
    fn from_path_go() {
        assert_eq!(
            LapceLanguage::from_path(&PathBuf::from("main.go")),
            LapceLanguage::Go,
        );
    }

    #[test]
    fn from_path_cpp_variants() {
        for ext in &["cpp", "cxx", "cc", "hpp", "hxx"] {
            let path = PathBuf::from(format!("file.{ext}"));
            assert_eq!(
                LapceLanguage::from_path(&path),
                LapceLanguage::Cpp,
                "failed for extension: {ext}"
            );
        }
    }

    #[test]
    fn from_path_case_insensitive_extension() {
        // Extensions are lowercased before matching
        assert_eq!(
            LapceLanguage::from_path(&PathBuf::from("file.RS")),
            LapceLanguage::Rust,
        );
        assert_eq!(
            LapceLanguage::from_path(&PathBuf::from("file.Py")),
            LapceLanguage::Python,
        );
    }

    #[test]
    fn from_path_by_filename() {
        assert_eq!(
            LapceLanguage::from_path(&PathBuf::from("Dockerfile")),
            LapceLanguage::Dockerfile,
        );
        assert_eq!(
            LapceLanguage::from_path(&PathBuf::from("Cargo.lock")),
            LapceLanguage::Toml,
        );
    }

    // --- from_path_raw returning None ---

    #[test]
    fn from_path_raw_unknown_extension() {
        assert_eq!(
            LapceLanguage::from_path_raw(&PathBuf::from("file.xyz")),
            None
        );
    }

    #[test]
    fn from_path_raw_no_extension() {
        // A file with no extension that isn't a known filename
        assert_eq!(
            LapceLanguage::from_path_raw(&PathBuf::from("randomfile")),
            None
        );
    }

    #[test]
    fn from_path_raw_known_returns_some() {
        assert_eq!(
            LapceLanguage::from_path_raw(&PathBuf::from("main.rs")),
            Some(LapceLanguage::Rust),
        );
    }

    #[test]
    fn from_path_unknown_defaults_to_plaintext() {
        assert_eq!(
            LapceLanguage::from_path(&PathBuf::from("file.xyz")),
            LapceLanguage::PlainText,
        );
    }

    // --- from_name ---

    #[test]
    fn from_name_exact_case() {
        assert_eq!(LapceLanguage::from_name("Rust"), Some(LapceLanguage::Rust),);
    }

    #[test]
    fn from_name_case_insensitive() {
        assert_eq!(LapceLanguage::from_name("rust"), Some(LapceLanguage::Rust),);
        assert_eq!(
            LapceLanguage::from_name("PYTHON"),
            Some(LapceLanguage::Python),
        );
        assert_eq!(
            LapceLanguage::from_name("JavaScript"),
            Some(LapceLanguage::Javascript),
        );
    }

    #[test]
    fn from_name_unknown() {
        assert_eq!(LapceLanguage::from_name("nonexistent_lang"), None);
    }

    // --- languages() ---

    #[test]
    fn languages_is_not_empty() {
        let langs = LapceLanguage::languages();
        assert!(!langs.is_empty());
    }

    #[test]
    fn languages_contains_well_known() {
        let langs = LapceLanguage::languages();
        assert!(langs.contains(&"Rust"), "missing Rust");
        assert!(langs.contains(&"Python"), "missing Python");
        assert!(langs.contains(&"JavaScript"), "missing JavaScript");
    }

    #[test]
    fn languages_excludes_inline_grammars() {
        // MarkdownInline has no message, so should not appear
        let langs = LapceLanguage::languages();
        for l in &langs {
            assert!(
                !l.contains("markdown.inline"),
                "inline grammar should not appear"
            );
        }
    }

    // --- comment_token ---

    #[test]
    fn comment_token_rust() {
        assert_eq!(LapceLanguage::Rust.comment_token(), "//");
    }

    #[test]
    fn comment_token_python() {
        assert_eq!(LapceLanguage::Python.comment_token(), "#");
    }

    #[test]
    fn comment_token_css() {
        assert_eq!(LapceLanguage::Css.comment_token(), "/*");
    }

    #[test]
    fn comment_token_plaintext_empty() {
        assert_eq!(LapceLanguage::PlainText.comment_token(), "");
    }

    // --- indent_unit ---

    #[test]
    fn indent_unit_rust_4spaces() {
        assert_eq!(LapceLanguage::Rust.indent_unit(), "    ");
    }

    #[test]
    fn indent_unit_bash_2spaces() {
        assert_eq!(LapceLanguage::Bash.indent_unit(), "  ");
    }

    #[test]
    fn indent_unit_go_tab() {
        assert_eq!(LapceLanguage::Go.indent_unit(), "\t");
    }

    #[test]
    fn indent_unit_hare_8spaces() {
        assert_eq!(LapceLanguage::Hare.indent_unit(), "        ");
        assert_eq!(LapceLanguage::Hare.indent_unit().len(), 8);
    }

    // --- name() tests ---

    #[test]
    fn name_rust() {
        assert_eq!(LapceLanguage::Rust.name(), "Rust");
    }

    #[test]
    fn name_cpp_uses_strum_message() {
        // Cpp has message "C++", so name() returns "C++" not "Cpp"
        assert_eq!(LapceLanguage::Cpp.name(), "C++");
    }

    #[test]
    fn name_csharp() {
        assert_eq!(LapceLanguage::Csharp.name(), "C#");
    }

    #[test]
    fn name_plain_text() {
        assert_eq!(LapceLanguage::PlainText.name(), "Plain Text");
    }

    #[test]
    fn name_javascript() {
        assert_eq!(LapceLanguage::Javascript.name(), "JavaScript");
    }

    #[test]
    fn name_markdown_inline_falls_back_to_variant() {
        // MarkdownInline has no #[strum(message)], so get_message returns None
        // and name() falls back to the variant ref string "markdown.inline"
        let name = LapceLanguage::MarkdownInline.name();
        assert_eq!(name, "markdown.inline");
    }

    #[test]
    fn name_ocaml_interface_falls_back() {
        // OcamlInterface has serialize="ocaml.interface" but no message
        let name = LapceLanguage::OcamlInterface.name();
        assert_eq!(name, "ocaml.interface");
    }

    // --- sticky_header_tags() tests ---

    #[test]
    fn sticky_header_tags_rust_non_empty() {
        let tags = LapceLanguage::Rust.sticky_header_tags();
        assert!(!tags.is_empty());
        assert!(tags.contains(&"function_item"));
        assert!(tags.contains(&"impl_item"));
    }

    #[test]
    fn sticky_header_tags_plaintext_empty() {
        let tags = LapceLanguage::PlainText.sticky_header_tags();
        assert!(tags.is_empty());
    }

    #[test]
    fn sticky_header_tags_ruby() {
        let tags = LapceLanguage::Ruby.sticky_header_tags();
        assert!(tags.contains(&"class"));
        assert!(tags.contains(&"method"));
        assert!(tags.contains(&"do_block"));
    }

    #[test]
    fn sticky_header_tags_c() {
        let tags = LapceLanguage::C.sticky_header_tags();
        assert!(tags.contains(&"function_definition"));
        assert!(tags.contains(&"struct_specifier"));
    }

    // --- query_name() tests ---

    #[test]
    fn query_name_default_uses_variant() {
        // Rust has no query override, so uses variant name lowercased
        assert_eq!(LapceLanguage::Rust.query_name(), "rust");
    }

    #[test]
    fn query_name_jsx_override() {
        // Jsx has query: Some("jsx")
        assert_eq!(LapceLanguage::Jsx.query_name(), "jsx");
    }

    #[test]
    fn query_name_tsx_override() {
        assert_eq!(LapceLanguage::Tsx.query_name(), "tsx");
    }

    #[test]
    fn query_name_typescript_override() {
        assert_eq!(LapceLanguage::Typescript.query_name(), "typescript");
    }

    #[test]
    fn query_name_markdown_inline_override() {
        assert_eq!(
            LapceLanguage::MarkdownInline.query_name(),
            "markdown.inline"
        );
    }

    // --- grammar_name() tests ---

    #[test]
    fn grammar_name_default() {
        assert_eq!(LapceLanguage::Rust.grammar_name(), "rust");
        assert_eq!(LapceLanguage::Python.grammar_name(), "python");
    }

    #[test]
    fn grammar_name_jsx_overrides_to_javascript() {
        // Jsx has grammar: Some("javascript")
        assert_eq!(LapceLanguage::Jsx.grammar_name(), "javascript");
    }

    #[test]
    fn grammar_name_tsx_override() {
        assert_eq!(LapceLanguage::Tsx.grammar_name(), "tsx");
    }

    #[test]
    fn grammar_name_markdown_inline() {
        assert_eq!(
            LapceLanguage::MarkdownInline.grammar_name(),
            "markdown_inline"
        );
    }

    // --- grammar_fn_name() tests ---

    #[test]
    fn grammar_fn_name_default() {
        assert_eq!(LapceLanguage::Rust.grammar_fn_name(), "rust");
    }

    #[test]
    fn grammar_fn_name_jsx_overrides_to_javascript() {
        assert_eq!(LapceLanguage::Jsx.grammar_fn_name(), "javascript");
    }

    #[test]
    fn grammar_fn_name_markdown_inline() {
        assert_eq!(
            LapceLanguage::MarkdownInline.grammar_fn_name(),
            "markdown_inline"
        );
    }

    // --- Indent tests ---

    #[test]
    fn indent_tab_is_tab_char() {
        assert_eq!(super::Indent::Tab.as_str(), "\t");
    }

    #[test]
    fn indent_space_2() {
        assert_eq!(super::Indent::Space(2).as_str(), "  ");
        assert_eq!(super::Indent::Space(2).as_str().len(), 2);
    }

    #[test]
    fn indent_space_4() {
        assert_eq!(super::Indent::Space(4).as_str(), "    ");
        assert_eq!(super::Indent::Space(4).as_str().len(), 4);
    }

    #[test]
    fn indent_space_8() {
        assert_eq!(super::Indent::Space(8).as_str(), "        ");
        assert_eq!(super::Indent::Space(8).as_str().len(), 8);
    }

    #[test]
    fn indent_space_unsupported_falls_to_8() {
        // Any unsupported count (e.g. 3) falls through to 8 spaces
        assert_eq!(super::Indent::Space(3).as_str().len(), 8);
        assert_eq!(super::Indent::Space(1).as_str().len(), 8);
        assert_eq!(super::Indent::Space(6).as_str().len(), 8);
    }

    // --- read_grammar_query() tests ---

    #[test]
    fn read_grammar_query_missing_file_returns_empty() {
        let dir = std::env::temp_dir().join("lapce_test_grammar_query_missing");
        let _ = std::fs::create_dir_all(&dir);
        let result =
            super::read_grammar_query(&dir, "nonexistent", "highlights.scm");
        assert!(result.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_grammar_query_simple_file() {
        let dir = std::env::temp_dir().join("lapce_test_grammar_query_simple");
        let lang_dir = dir.join("testlang");
        let _ = std::fs::create_dir_all(&lang_dir);
        std::fs::write(lang_dir.join("highlights.scm"), "(identifier) @variable")
            .unwrap();
        let result = super::read_grammar_query(&dir, "testlang", "highlights.scm");
        assert_eq!(result, "(identifier) @variable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_grammar_query_inherits_directive() {
        let dir = std::env::temp_dir().join("lapce_test_grammar_query_inherits");
        let base_dir = dir.join("base");
        let child_dir = dir.join("child");
        let _ = std::fs::create_dir_all(&base_dir);
        let _ = std::fs::create_dir_all(&child_dir);

        std::fs::write(base_dir.join("highlights.scm"), "(number) @number").unwrap();
        std::fs::write(
            child_dir.join("highlights.scm"),
            ";; inherits: base\n(string) @string",
        )
        .unwrap();

        let result = super::read_grammar_query(&dir, "child", "highlights.scm");
        assert!(result.contains("(number) @number"));
        assert!(result.contains("(string) @string"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- properties() integrity test ---

    #[test]
    fn properties_array_integrity() {
        // Verify every variant's properties().id matches the variant
        // This catches any accidental mis-ordering in the LANGUAGES array

        // We verify known variants (no IntoEnumIterator available)
        let variants = [
            LapceLanguage::PlainText,
            LapceLanguage::Rust,
            LapceLanguage::Python,
            LapceLanguage::Javascript,
            LapceLanguage::Typescript,
            LapceLanguage::Go,
            LapceLanguage::C,
            LapceLanguage::Cpp,
            LapceLanguage::Java,
            LapceLanguage::Ruby,
            LapceLanguage::Bash,
            LapceLanguage::Jsx,
            LapceLanguage::Tsx,
            LapceLanguage::Markdown,
            LapceLanguage::MarkdownInline,
            LapceLanguage::Json,
            LapceLanguage::Html,
            LapceLanguage::Css,
            LapceLanguage::Toml,
            LapceLanguage::Yaml,
        ];
        for variant in &variants {
            let props = variant.properties();
            assert_eq!(
                props.id, *variant,
                "LANGUAGES array mismatch for {:?}",
                variant
            );
        }
    }

    // --- add_bracket_pos tests ---

    #[test]
    fn add_bracket_pos_creates_entry() {
        use std::collections::HashMap;
        use tree_sitter::Point;

        let mut bracket_pos = HashMap::new();
        super::add_bracket_pos(
            &mut bracket_pos,
            Point::new(5, 10),
            "bracket.color1",
        );
        assert!(bracket_pos.contains_key(&5));
        let styles = &bracket_pos[&5];
        assert_eq!(styles.len(), 1);
        assert_eq!(styles[0].start, 10);
        assert_eq!(styles[0].end, 11);
        assert_eq!(styles[0].style.fg_color.as_deref(), Some("bracket.color1"));
    }

    #[test]
    fn add_bracket_pos_appends_to_existing() {
        use std::collections::HashMap;
        use tree_sitter::Point;

        let mut bracket_pos = HashMap::new();
        super::add_bracket_pos(&mut bracket_pos, Point::new(3, 0), "red");
        super::add_bracket_pos(&mut bracket_pos, Point::new(3, 5), "blue");
        let styles = &bracket_pos[&3];
        assert_eq!(styles.len(), 2);
        assert_eq!(styles[0].start, 0);
        assert_eq!(styles[1].start, 5);
    }

    #[test]
    fn no_duplicate_extensions_in_any_language() {
        use std::collections::HashSet;

        for lang in super::LANGUAGES {
            let mut seen = HashSet::new();
            for ext in lang.extensions {
                assert!(
                    seen.insert(*ext),
                    "Language {:?} has duplicate extension: {:?}",
                    lang.id,
                    ext
                );
            }
        }
    }

    #[test]
    fn properties_matches_all_variants() {
        // Verify that calling properties() on every variant succeeds
        // (the assert inside properties() checks array ordering)
        for lang in super::LANGUAGES {
            let props = lang.id.properties();
            assert_eq!(props.id, lang.id, "properties() mismatch for {:?}", lang.id);
        }
    }
}
