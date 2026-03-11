use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};

/// A column in a database table, as parsed from schema.rb.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaColumn {
    pub name: String,
    pub col_type: String,
    pub null: bool,
    pub default: Option<String>,
    /// 1-based line number in schema.rb
    pub line: usize,
}

/// An index on a database table, as parsed from schema.rb.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaIndex {
    pub columns: Vec<String>,
    pub name: Option<String>,
    pub unique: bool,
    /// 1-based line number in schema.rb
    pub line: usize,
}

/// A table definition parsed from schema.rb.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaTable {
    pub name: String,
    pub columns: Vec<SchemaColumn>,
    pub indexes: Vec<SchemaIndex>,
}

/// All schema information for a Rails project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaInfo {
    pub project_root: PathBuf,
    pub tables: HashMap<String, SchemaTable>,
}

/// Given a model file path relative to the project root, try to find the
/// matching table name from the schema.
///
/// Looks for `app/models/` anywhere in the path to support monorepo layouts like:
/// - `app/models/user.rb` → table `users`
/// - `app/models/blog_post.rb` → table `blog_posts`
/// - `app/models/admin/user.rb` → table `admin_users` or `users`
/// - `components/expenses/app/models/expenses/expense.rb` → table `expenses`
pub fn model_path_to_table_name(
    relative_path: &std::path::Path,
    tables: &HashMap<String, SchemaTable>,
) -> Option<String> {
    // Find app/models/ anywhere in the path (supports monorepo layouts)
    let path_str = relative_path.to_string_lossy();
    let model_path = path_str
        .find("app/models/")
        .map(|pos| &path_str[pos + "app/models/".len()..])
        .and_then(|s| s.strip_suffix(".rb"))?;

    // Try namespace_model → namespace_models (e.g. admin/user → admin_users)
    let underscored = model_path.replace('/', "_");
    let pluralized = simple_pluralize(&underscored);
    if tables.contains_key(&pluralized) {
        return Some(pluralized);
    }

    // Try just the filename part (e.g. admin/user → users)
    if let Some(filename) = model_path.rsplit('/').next() {
        let pluralized = simple_pluralize(filename);
        if tables.contains_key(&pluralized) {
            return Some(pluralized);
        }
    }

    // Try exact match (singular table names, though rare)
    if tables.contains_key(&underscored) {
        return Some(underscored);
    }

    None
}

/// Simple English pluralization covering common Rails model name patterns.
fn simple_pluralize(s: &str) -> String {
    if s.ends_with("ies") || s.ends_with("ses") || s.ends_with("xes") {
        return s.to_string();
    }
    if s.ends_with('y') {
        if let Some(prefix) = s.strip_suffix('y') {
            let last_char = prefix.chars().last();
            if last_char.is_some_and(|c| !"aeiou".contains(c)) {
                return format!("{prefix}ies");
            }
        }
    }
    if s.ends_with('s') || s.ends_with('x') || s.ends_with("ch") || s.ends_with("sh")
    {
        return format!("{s}es");
    }
    format!("{s}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_pluralize() {
        assert_eq!(simple_pluralize("user"), "users");
        assert_eq!(simple_pluralize("post"), "posts");
        assert_eq!(simple_pluralize("category"), "categories");
        assert_eq!(simple_pluralize("bus"), "buses");
        assert_eq!(simple_pluralize("box"), "boxes");
        assert_eq!(simple_pluralize("key"), "keys");
        assert_eq!(simple_pluralize("blog_post"), "blog_posts");
    }

    #[test]
    fn test_model_path_to_table_name() {
        let mut tables = HashMap::new();
        tables.insert(
            "users".to_string(),
            SchemaTable {
                name: "users".to_string(),
                columns: vec![],
                indexes: vec![],
            },
        );
        tables.insert(
            "blog_posts".to_string(),
            SchemaTable {
                name: "blog_posts".to_string(),
                columns: vec![],
                indexes: vec![],
            },
        );
        tables.insert(
            "admin_users".to_string(),
            SchemaTable {
                name: "admin_users".to_string(),
                columns: vec![],
                indexes: vec![],
            },
        );

        assert_eq!(
            model_path_to_table_name(
                std::path::Path::new("app/models/user.rb"),
                &tables
            ),
            Some("users".to_string())
        );
        assert_eq!(
            model_path_to_table_name(
                std::path::Path::new("app/models/blog_post.rb"),
                &tables
            ),
            Some("blog_posts".to_string())
        );
        assert_eq!(
            model_path_to_table_name(
                std::path::Path::new("app/models/admin/user.rb"),
                &tables
            ),
            Some("admin_users".to_string())
        );
        assert_eq!(
            model_path_to_table_name(
                std::path::Path::new("app/models/unknown.rb"),
                &tables
            ),
            None
        );

        // Monorepo: components/expenses/app/models/expenses/expense.rb
        tables.insert(
            "expenses".to_string(),
            SchemaTable {
                name: "expenses".to_string(),
                columns: vec![],
                indexes: vec![],
            },
        );
        assert_eq!(
            model_path_to_table_name(
                std::path::Path::new(
                    "components/expenses/app/models/expenses/expense.rb"
                ),
                &tables
            ),
            Some("expenses".to_string())
        );

        // Monorepo: components/core/app/models/user.rb
        assert_eq!(
            model_path_to_table_name(
                std::path::Path::new("components/core/app/models/user.rb"),
                &tables
            ),
            Some("users".to_string())
        );

        // Not a model file at all
        assert_eq!(
            model_path_to_table_name(
                std::path::Path::new("lib/some_file.rb"),
                &tables
            ),
            None
        );
    }
}
