//! Field path expressions for nested JSON values.
//!
//! A path is a dot-separated list of field names: `address.city`,
//! `tags.0.name` (numeric segments index into arrays). Empty path returns
//! the whole document.

use serde_json::Value;

/// A parsed field path. Owned and cheap to clone.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Path {
    segments: Vec<String>,
}

impl Path {
    /// Parse a dotted path string. Empty string means "the whole document".
    pub fn parse(s: &str) -> Self {
        if s.is_empty() {
            return Self {
                segments: Vec::new(),
            };
        }
        Self {
            segments: s.split('.').map(|seg| seg.to_owned()).collect(),
        }
    }

    /// Iterate the path segments as strings.
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// Render the path back to dotted form.
    pub fn to_dotted(&self) -> String {
        self.segments.join(".")
    }

    /// Look up the value at this path inside `value`. Returns `Null` for
    /// missing paths (matching JSON path semantics in most engines).
    pub fn lookup<'a>(&self, value: &'a Value) -> &'a Value {
        let mut current = value;
        for segment in &self.segments {
            match current {
                Value::Object(map) => match map.get(segment) {
                    Some(child) => current = child,
                    None => return &Value::Null,
                },
                Value::Array(arr) => {
                    let idx: usize = match segment.parse() {
                        Ok(idx) => idx,
                        Err(_) => return &Value::Null,
                    };
                    match arr.get(idx) {
                        Some(child) => current = child,
                        None => return &Value::Null,
                    }
                }
                _ => return &Value::Null,
            }
        }
        current
    }

    /// Set the value at this path. Creates intermediate objects as needed.
    /// Returns a new owned `Value` rather than mutating in place.
    pub fn set(&self, mut root: Value, new_value: Value) -> Value {
        if self.segments.is_empty() {
            return new_value;
        }
        set_recursive(&mut root, &self.segments, new_value);
        root
    }

    /// Remove the value at this path. Returns `true` if something was removed.
    pub fn remove(&self, root: &mut Value) -> bool {
        if self.segments.is_empty() {
            return false;
        }
        remove_recursive(root, &self.segments)
    }
}

fn set_recursive(value: &mut Value, segments: &[String], new_value: Value) {
    if segments.is_empty() {
        *value = new_value;
        return;
    }
    let segment = &segments[0];
    let rest = &segments[1..];

    // Coerce non-object/non-array into an object so we can descend.
    if !value.is_object() && !value.is_array() {
        *value = Value::Object(serde_json::Map::new());
    }

    if value.is_object() {
        let map = value.as_object_mut().unwrap();
        if !map.contains_key(segment) {
            map.insert(segment.clone(), Value::Null);
        }
        if rest.is_empty() {
            map.insert(segment.clone(), new_value);
        } else {
            let child = map.get_mut(segment).unwrap();
            set_recursive(child, rest, new_value);
        }
    } else if value.is_array() {
        let idx: usize = match segment.parse() {
            Ok(idx) => idx,
            Err(_) => return,
        };
        let arr = value.as_array_mut().unwrap();
        while arr.len() <= idx {
            arr.push(Value::Null);
        }
        if rest.is_empty() {
            arr[idx] = new_value;
        } else {
            set_recursive(&mut arr[idx], rest, new_value);
        }
    }
}

fn remove_recursive(value: &mut Value, segments: &[String]) -> bool {
    if segments.is_empty() {
        return false;
    }
    let segment = &segments[0];
    let rest = &segments[1..];

    match value {
        Value::Object(map) => {
            if rest.is_empty() {
                map.remove(segment).is_some()
            } else if let Some(child) = map.get_mut(segment) {
                remove_recursive(child, rest)
            } else {
                false
            }
        }
        Value::Array(arr) => {
            let idx: usize = match segment.parse() {
                Ok(idx) => idx,
                Err(_) => return false,
            };
            if idx >= arr.len() {
                return false;
            }
            if rest.is_empty() {
                arr.remove(idx);
                true
            } else {
                remove_recursive(&mut arr[idx], rest)
            }
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_dotted_path() {
        let p = Path::parse("a.b.c");
        assert_eq!(p.segments(), &["a", "b", "c"]);
        assert_eq!(p.to_dotted(), "a.b.c");
    }

    #[test]
    fn empty_path_returns_whole_value() {
        let p = Path::parse("");
        let v = json!({"x": 1});
        assert_eq!(p.lookup(&v), &v);
    }

    #[test]
    fn looks_up_nested_object() {
        let v = json!({"user": {"name": "Alice", "address": {"city": "Nairobi"}}});
        assert_eq!(Path::parse("user.name").lookup(&v), &json!("Alice"));
        assert_eq!(
            Path::parse("user.address.city").lookup(&v),
            &json!("Nairobi")
        );
        assert_eq!(Path::parse("user.missing").lookup(&v), &Value::Null);
        assert_eq!(Path::parse("user.address.country").lookup(&v), &Value::Null);
    }

    #[test]
    fn looks_up_array_index() {
        let v = json!({"tags": ["red", "green", "blue"]});
        assert_eq!(Path::parse("tags.0").lookup(&v), &json!("red"));
        assert_eq!(Path::parse("tags.2").lookup(&v), &json!("blue"));
        assert_eq!(Path::parse("tags.5").lookup(&v), &Value::Null);
    }

    #[test]
    fn sets_nested_object_creating_intermediates() {
        let v = json!({});
        let result = Path::parse("a.b.c").set(v, json!(42));
        assert_eq!(result, json!({"a": {"b": {"c": 42}}}));
    }

    #[test]
    fn removes_leaf() {
        let mut v = json!({"a": {"b": 1, "c": 2}});
        assert!(Path::parse("a.b").remove(&mut v));
        assert_eq!(v, json!({"a": {"c": 2}}));
        assert!(!Path::parse("a.missing").remove(&mut v));
    }
}
