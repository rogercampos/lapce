use std::{collections::HashMap, path::Path};

use lapce_rpc::schema::{SchemaColumn, SchemaIndex, SchemaTable};

/// Parse a Rails `db/schema.rb` file and extract table definitions.
///
/// This is a simple line-based parser that handles the common schema.rb format:
/// ```ruby
/// create_table "users", force: :cascade do |t|
///   t.string "email", null: false
///   t.integer "age", default: 0
///   t.timestamps
/// end
///
/// add_index "users", ["email"], name: "index_users_on_email", unique: true
/// ```
pub fn parse_schema_rb(path: &Path) -> Option<HashMap<String, SchemaTable>> {
    let content = std::fs::read_to_string(path).ok()?;
    Some(parse_schema_content(&content))
}

fn parse_schema_content(content: &str) -> HashMap<String, SchemaTable> {
    let mut tables: HashMap<String, SchemaTable> = HashMap::new();
    let mut current_table: Option<String> = None;

    for (line_idx, line) in content.lines().enumerate() {
        let line_number = line_idx + 1; // 1-based
        let trimmed = line.trim();

        // Match: create_table "table_name" ...
        if let Some(table_name) = parse_create_table(trimmed) {
            current_table = Some(table_name.clone());
            tables
                .entry(table_name.clone())
                .or_insert_with(|| SchemaTable {
                    name: table_name,
                    columns: Vec::new(),
                    indexes: Vec::new(),
                });
            continue;
        }

        // Match end of create_table block
        if trimmed == "end" && current_table.is_some() {
            current_table = None;
            continue;
        }

        // Match column or inline index definition inside create_table block
        if let Some(ref table_name) = current_table {
            if let Some(index) = parse_inline_index(trimmed, line_number) {
                if let Some(table) = tables.get_mut(table_name) {
                    table.indexes.push(index);
                }
            } else if let Some(column) = parse_column(trimmed, line_number) {
                if let Some(table) = tables.get_mut(table_name) {
                    table.columns.push(column);
                }
            }
            continue;
        }

        // Match: add_index "table_name", ["col1", "col2"], ...
        if let Some((table_name, index)) = parse_add_index(trimmed, line_number) {
            if let Some(table) = tables.get_mut(&table_name) {
                table.indexes.push(index);
            }
        }
    }

    tables
}

/// Parse `create_table "name"` and return the table name.
fn parse_create_table(line: &str) -> Option<String> {
    let rest = line.strip_prefix("create_table")?;
    extract_first_quoted_string(rest)
}

/// Parse a column definition like `t.string "email", null: false, default: ""`
fn parse_column(line: &str, line_number: usize) -> Option<SchemaColumn> {
    // Must start with t.
    let rest = line.strip_prefix("t.")?;

    // Handle t.timestamps specially
    if rest.starts_with("timestamps") {
        return None; // timestamps are implicit columns, skip
    }

    // Extract type: first word before space
    let (col_type, rest) = rest.split_once(|c: char| c.is_whitespace())?;
    let col_type = col_type.trim_end_matches(',').to_string();

    // Extract column name (first quoted string)
    let name = extract_first_quoted_string(rest)?;

    // Parse options
    let null = !rest.contains("null: false");
    let default = extract_option_value(rest, "default:");

    Some(SchemaColumn {
        name,
        col_type,
        null,
        default,
        line: line_number,
    })
}

/// Parse `add_index "table", ["col1", "col2"], name: "idx", unique: true`
fn parse_add_index(line: &str, line_number: usize) -> Option<(String, SchemaIndex)> {
    let rest = line.strip_prefix("add_index")?;

    // Extract table name
    let table_name = extract_first_quoted_string(rest)?;

    // Find the array of columns: ["col1", "col2"]
    let columns = extract_string_array(rest);

    let name = extract_option_value(rest, "name:");
    let unique = rest.contains("unique: true");

    Some((
        table_name,
        SchemaIndex {
            columns,
            name,
            unique,
            line: line_number,
        },
    ))
}

/// Parse an inline index like `t.index ["col1", "col2"], name: "idx", unique: true`
fn parse_inline_index(line: &str, line_number: usize) -> Option<SchemaIndex> {
    let rest = line.strip_prefix("t.index")?;
    // Must be followed by whitespace or [ to avoid matching t.index_something
    if !rest.starts_with(|c: char| c.is_whitespace() || c == '[') {
        return None;
    }

    let columns = extract_string_array(rest);
    let name = extract_option_value(rest, "name:");
    let unique = rest.contains("unique: true");

    Some(SchemaIndex {
        columns,
        name,
        unique,
        line: line_number,
    })
}

/// Extract the first double-quoted string from text.
fn extract_first_quoted_string(text: &str) -> Option<String> {
    let start = text.find('"')? + 1;
    let end = start + text[start..].find('"')?;
    Some(text[start..end].to_string())
}

/// Extract an array of quoted strings like ["a", "b"] from text.
fn extract_string_array(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let Some(start) = text.find('[') else {
        return result;
    };
    let Some(end) = text[start..].find(']') else {
        return result;
    };
    let array_content = &text[start + 1..start + end];

    let mut pos = 0;
    while pos < array_content.len() {
        if let Some(q_start) = array_content[pos..].find('"') {
            let abs_start = pos + q_start + 1;
            if let Some(q_end) = array_content[abs_start..].find('"') {
                result.push(array_content[abs_start..abs_start + q_end].to_string());
                pos = abs_start + q_end + 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }

    result
}

/// Extract a Ruby keyword argument value like `default: "foo"` or `default: 0`.
fn extract_option_value(text: &str, key: &str) -> Option<String> {
    let key_pos = text.find(key)?;
    let after_key = text[key_pos + key.len()..].trim_start();

    if after_key.starts_with('"') {
        // Quoted string value
        extract_first_quoted_string(after_key)
    } else {
        // Unquoted value (number, symbol, boolean)
        let value = after_key
            .split(|c: char| c == ',' || c.is_whitespace())
            .next()?;
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_schema() {
        let content = r#"
ActiveRecord::Schema[7.1].define(version: 2024_01_15_000000) do

  create_table "users", force: :cascade do |t|
    t.string "email", null: false
    t.string "name"
    t.boolean "admin", default: false, null: false
    t.integer "age"
    t.timestamps
  end

  add_index "users", ["email"], name: "index_users_on_email", unique: true
  add_index "users", ["name"], name: "index_users_on_name"

  create_table "posts", force: :cascade do |t|
    t.string "title", null: false
    t.text "body"
    t.bigint "user_id", null: false
    t.timestamps
  end

  add_index "posts", ["user_id"], name: "index_posts_on_user_id"

end
"#;

        let tables = parse_schema_content(content);

        // Check users table
        let users = tables.get("users").unwrap();
        assert_eq!(users.columns.len(), 4);
        assert_eq!(users.columns[0].name, "email");
        assert_eq!(users.columns[0].col_type, "string");
        assert!(!users.columns[0].null);
        assert_eq!(users.columns[1].name, "name");
        assert!(users.columns[1].null);
        assert_eq!(users.columns[2].name, "admin");
        assert_eq!(users.columns[2].col_type, "boolean");
        assert!(!users.columns[2].null);
        assert_eq!(users.columns[2].default, Some("false".to_string()));

        // Check indexes
        assert_eq!(users.indexes.len(), 2);
        assert_eq!(users.indexes[0].columns, vec!["email"]);
        assert!(users.indexes[0].unique);
        assert_eq!(users.indexes[1].columns, vec!["name"]);
        assert!(!users.indexes[1].unique);

        // Check posts table
        let posts = tables.get("posts").unwrap();
        assert_eq!(posts.columns.len(), 3);
        assert_eq!(posts.indexes.len(), 1);
    }

    #[test]
    fn test_parse_inline_indexes() {
        let content = r#"
  create_table "users", force: :cascade do |t|
    t.string "email", null: false
    t.index ["email"], name: "index_users_on_email", unique: true
    t.index ["company_id"], name: "index_users_on_company_id"
    t.index ["first_name", "last_name"], name: "index_users_on_names"
  end
"#;
        let tables = parse_schema_content(content);
        let users = tables.get("users").unwrap();
        assert_eq!(users.indexes.len(), 3);
        assert_eq!(users.indexes[0].columns, vec!["email"]);
        assert!(users.indexes[0].unique);
        assert_eq!(users.indexes[1].columns, vec!["company_id"]);
        assert!(!users.indexes[1].unique);
        assert_eq!(users.indexes[2].columns, vec!["first_name", "last_name"]);
    }

    #[test]
    fn test_extract_string_array() {
        assert_eq!(
            extract_string_array(r#"["email", "name"]"#),
            vec!["email", "name"]
        );
        assert_eq!(extract_string_array(r#"["email"]"#), vec!["email"]);
        assert_eq!(extract_string_array("no array here"), Vec::<String>::new());
    }

    #[test]
    fn test_extract_option_value() {
        assert_eq!(
            extract_option_value(r#"default: "hello""#, "default:"),
            Some("hello".to_string())
        );
        assert_eq!(
            extract_option_value("default: false, null: false", "default:"),
            Some("false".to_string())
        );
        assert_eq!(
            extract_option_value("default: 42, null: false", "default:"),
            Some("42".to_string())
        );
        assert_eq!(extract_option_value("null: false", "default:"), None);
    }
}
