//! Filter predicates for document queries.
//!
//! A filter is a JSON value describing a predicate. The engine evaluates
//! each document against the filter and includes it if the filter matches.
//!
//! ## Examples
//!
//! ```text
//! // Match documents where "name" equals "Alice"
//! {"name": {"$eq": "Alice"}}
//!
//! // Match documents where "age" is greater than 18
//! {"age": {"$gt": 18}}
//!
//! // Match documents where "tags" contains "vip"
//! {"tags": {"$contains": "vip"}}
//!
//! // Top-level AND of multiple field filters
//! {"name": {"$eq": "Alice"}, "age": {"$gt": 18}}
//!
//! // Explicit logical operators
//! {"$or": [{"name": {"$eq": "Alice"}}, {"age": {"$gt": 18}}]}
//! ```

use serde_json::Value;

/// A parsed filter predicate.
#[derive(Debug, Clone)]
pub enum Filter {
    /// Match all documents.
    All,
    /// Match documents where `path` matches `op` and `operand`.
    Comparison {
        path: super::path::Path,
        op: Operator,
        operand: Value,
    },
    /// Match documents where all sub-filters match.
    And(Vec<Filter>),
    /// Match documents where any sub-filter matches.
    Or(Vec<Filter>),
    /// Match documents where the sub-filter does not match.
    Not(Box<Filter>),
}

/// Comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    /// `=` — equal.
    Eq,
    /// `!=` — not equal.
    NotEq,
    /// `<`
    Lt,
    /// `<=`
    LtEq,
    /// `>`
    Gt,
    /// `>=`
    GtEq,
    /// Membership test for arrays/strings.
    Contains,
    /// Array length > 0 (operand ignored).
    Exists,
}

impl Filter {
    /// Parse a filter from a JSON value. Returns `Err` if the filter is
    /// malformed.
    pub fn parse(value: &Value) -> Result<Self, FilterError> {
        match value {
            Value::Object(map) => {
                // Special operators first.
                if let Some(and_value) = map.get("$and") {
                    let sub = parse_filter_list(and_value)?;
                    return Ok(Self::And(sub));
                }
                if let Some(or_value) = map.get("$or") {
                    let sub = parse_filter_list(or_value)?;
                    return Ok(Self::Or(sub));
                }
                if let Some(not_value) = map.get("$not") {
                    let sub = Self::parse(not_value)?;
                    return Ok(Self::Not(Box::new(sub)));
                }

                // Otherwise: AND of {field: {op: value}} pairs.
                let mut clauses: Vec<Self> = Vec::with_capacity(map.len());
                for (field, expr) in map {
                    let path = super::path::Path::parse(field);
                    let clause = match expr {
                        Value::Object(op_map) => {
                            // Each entry is {op_name: operand}.
                            let mut clauses_for_field: Vec<Self> = Vec::new();
                            for (op_name, operand) in op_map {
                                let op = parse_operator(op_name)?;
                                clauses_for_field.push(Self::Comparison {
                                    path: path.clone(),
                                    op,
                                    operand: operand.clone(),
                                });
                            }
                            if clauses_for_field.len() == 1 {
                                clauses_for_field.pop().unwrap()
                            } else {
                                Self::And(clauses_for_field)
                            }
                        }
                        literal => Self::Comparison {
                            path,
                            op: Operator::Eq,
                            operand: literal.clone(),
                        },
                    };
                    clauses.push(clause);
                }
                if clauses.is_empty() {
                    Ok(Self::All)
                } else if clauses.len() == 1 {
                    Ok(clauses.pop().unwrap())
                } else {
                    Ok(Self::And(clauses))
                }
            }
            Value::Null => Ok(Self::All),
            other => Err(FilterError::InvalidFilterShape(other.to_string())),
        }
    }

    /// Test whether a document matches this filter.
    pub fn matches(&self, document: &Value) -> bool {
        match self {
            Self::All => true,
            Self::Comparison { path, op, operand } => {
                let value = path.lookup(document);
                apply_operator(*op, value, operand)
            }
            Self::And(filters) => filters.iter().all(|f| f.matches(document)),
            Self::Or(filters) => filters.iter().any(|f| f.matches(document)),
            Self::Not(filter) => !filter.matches(document),
        }
    }
}

fn parse_filter_list(value: &Value) -> Result<Vec<Filter>, FilterError> {
    match value {
        Value::Array(items) => items.iter().map(Filter::parse).collect(),
        _ => Err(FilterError::InvalidFilterShape(
            "$and/$or expects an array".to_owned(),
        )),
    }
}

fn parse_operator(name: &str) -> Result<Operator, FilterError> {
    Ok(match name {
        "$eq" => Operator::Eq,
        "$ne" => Operator::NotEq,
        "$lt" => Operator::Lt,
        "$lte" => Operator::LtEq,
        "$gt" => Operator::Gt,
        "$gte" => Operator::GtEq,
        "$contains" => Operator::Contains,
        "$exists" => Operator::Exists,
        other => {
            return Err(FilterError::UnknownOperator(other.to_owned()));
        }
    })
}

fn apply_operator(op: Operator, value: &Value, operand: &Value) -> bool {
    match op {
        Operator::Eq => value == operand,
        Operator::NotEq => value != operand,
        Operator::Lt => compare_json(value, operand) == Some(std::cmp::Ordering::Less),
        Operator::LtEq => matches!(
            compare_json(value, operand),
            Some(std::cmp::Ordering::Less) | Some(std::cmp::Ordering::Equal)
        ),
        Operator::Gt => compare_json(value, operand) == Some(std::cmp::Ordering::Greater),
        Operator::GtEq => matches!(
            compare_json(value, operand),
            Some(std::cmp::Ordering::Greater) | Some(std::cmp::Ordering::Equal)
        ),
        Operator::Contains => match (value, operand) {
            (Value::Array(arr), _) => arr.contains(operand),
            (Value::String(s), Value::String(sub)) => s.contains(sub),
            (Value::Object(map), Value::String(key)) => map.contains_key(key),
            (Value::String(s), _) => s.contains(&operand.to_string()),
            _ => false,
        },
        Operator::Exists => !matches!(value, Value::Null),
    }
}

/// Compare two JSON values by SQLite-like ordering: NULL < BOOL < NUMBER <
/// TEXT < BLOB, with structured values compared elementwise.
fn compare_json(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    match (left, right) {
        (Value::Null, Value::Null) => Some(Ordering::Equal),
        (Value::Null, _) => Some(Ordering::Less),
        (_, Value::Null) => Some(Ordering::Greater),
        (Value::Number(a), Value::Number(b)) => {
            let av = a.as_f64()?;
            let bv = b.as_f64()?;
            av.partial_cmp(&bv)
        }
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Array(a), Value::Array(b)) => compare_arrays(a, b),
        (Value::Object(a), Value::Object(b)) => compare_objects(a, b),
        _ => None,
    }
}

fn compare_arrays(a: &[Value], b: &[Value]) -> Option<std::cmp::Ordering> {
    for (l, r) in a.iter().zip(b.iter()) {
        match compare_json(l, r) {
            Some(std::cmp::Ordering::Equal) => continue,
            other => return other,
        }
    }
    Some(a.len().cmp(&b.len()))
}

fn compare_objects(
    a: &serde_json::Map<String, Value>,
    b: &serde_json::Map<String, Value>,
) -> Option<std::cmp::Ordering> {
    let a_keys: Vec<&String> = a.keys().collect();
    let b_keys: Vec<&String> = b.keys().collect();
    for (ak, bk) in a_keys.iter().zip(b_keys.iter()) {
        match ak.cmp(bk) {
            std::cmp::Ordering::Equal => match compare_json(a.get(*ak)?, b.get(*bk)?) {
                Some(std::cmp::Ordering::Equal) => continue,
                other => return other,
            },
            other => return Some(other),
        }
    }
    Some(a_keys.len().cmp(&b_keys.len()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterError {
    InvalidFilterShape(String),
    UnknownOperator(String),
}

impl std::fmt::Display for FilterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFilterShape(msg) => write!(f, "invalid filter shape: {msg}"),
            Self::UnknownOperator(op) => write!(f, "unknown operator: {op}"),
        }
    }
}

impl std::error::Error for FilterError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_equality_shorthand() {
        let f = Filter::parse(&json!({"name": "Alice"})).unwrap();
        assert!(f.matches(&json!({"name": "Alice"})));
        assert!(!f.matches(&json!({"name": "Bob"})));
        assert!(!f.matches(&json!({"age": 30})));
    }

    #[test]
    fn parses_explicit_operator() {
        let f = Filter::parse(&json!({"age": {"$gt": 18}})).unwrap();
        assert!(f.matches(&json!({"age": 30})));
        assert!(!f.matches(&json!({"age": 18})));
        assert!(!f.matches(&json!({"age": 10})));
    }

    #[test]
    fn parses_and_of_fields() {
        let f = Filter::parse(&json!({"name": "Alice", "age": {"$gt": 18}})).unwrap();
        assert!(f.matches(&json!({"name": "Alice", "age": 30})));
        assert!(!f.matches(&json!({"name": "Alice", "age": 10})));
        assert!(!f.matches(&json!({"name": "Bob", "age": 30})));
    }

    #[test]
    fn parses_or() {
        let f = Filter::parse(&json!({"$or": [{"name": "Alice"}, {"age": {"$gt": 50}}]})).unwrap();
        assert!(f.matches(&json!({"name": "Alice", "age": 10})));
        assert!(f.matches(&json!({"name": "Bob", "age": 60})));
        assert!(!f.matches(&json!({"name": "Bob", "age": 30})));
    }

    #[test]
    fn parses_not() {
        let f = Filter::parse(&json!({"$not": {"name": "Alice"}})).unwrap();
        assert!(!f.matches(&json!({"name": "Alice"})));
        assert!(f.matches(&json!({"name": "Bob"})));
    }

    #[test]
    fn contains_in_array() {
        let f = Filter::parse(&json!({"tags": {"$contains": "vip"}})).unwrap();
        assert!(f.matches(&json!({"tags": ["vip", "user"]})));
        assert!(!f.matches(&json!({"tags": ["user"]})));
    }

    #[test]
    fn nested_path_lookup() {
        let f = Filter::parse(&json!({"address.city": "Nairobi"})).unwrap();
        assert!(f.matches(&json!({"address": {"city": "Nairobi"}})));
        assert!(!f.matches(&json!({"address": {"city": "Lagos"}})));
    }

    #[test]
    fn empty_filter_matches_everything() {
        let f = Filter::parse(&json!({})).unwrap();
        assert!(f.matches(&json!({})));
        assert!(f.matches(&json!({"any": "thing"})));
    }
}
