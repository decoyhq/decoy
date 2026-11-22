//! JSON ⇄ msgpack for values crossing to a worker. Cases and templates work on JSON; a worker also
//! sees and returns wire bytes, as msgpack `bin`.

use std::fmt;

use rmpv::Value as Msgpack;
use serde_json::{Map, Number, Value};

pub fn to_msgpack(value: &Value) -> Msgpack {
    match value {
        Value::Null => Msgpack::Nil,
        Value::Bool(flag) => Msgpack::Boolean(*flag),
        Value::Number(number) => match (number.as_u64(), number.as_i64(), number.as_f64()) {
            (Some(unsigned), _, _) => Msgpack::from(unsigned),
            (_, Some(signed), _) => Msgpack::from(signed),
            (_, _, float) => Msgpack::F64(float.unwrap_or_default()),
        },
        Value::String(text) => Msgpack::from(text.as_str()),
        Value::Array(items) => Msgpack::Array(items.iter().map(to_msgpack).collect()),
        Value::Object(fields) => Msgpack::Map(
            fields
                .iter()
                .map(|(key, value)| (Msgpack::from(key.as_str()), to_msgpack(value)))
                .collect(),
        ),
    }
}

/// The JSON a worker's value stands for. Bytes, non-string map keys, non-UTF-8 strings and
/// extension types have none; the error names where the first one is (`$.data`).
pub fn to_json(value: &Msgpack) -> Result<Value, String> {
    at(value, &Path::Root)
}

/// Keep path segments on the stack; only format them if conversion fails.
enum Path<'a> {
    Root,
    Item(&'a Self, usize),
    Field(&'a Self, &'a str),
}

impl fmt::Display for Path<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Conversion is still on the stack, so render the parent chain iteratively.
        let mut segments = Vec::new();
        let mut segment = self;
        loop {
            segments.push(segment);
            segment = match segment {
                Self::Root => break,
                Self::Item(parent, _) | Self::Field(parent, _) => parent,
            };
        }
        for segment in segments.into_iter().rev() {
            match segment {
                Self::Root => f.write_str("$")?,
                Self::Item(_, index) => write!(f, "[{index}]")?,
                Self::Field(_, key) => write!(f, ".{key}")?,
            }
        }
        Ok(())
    }
}

fn at(value: &Msgpack, path: &Path<'_>) -> Result<Value, String> {
    Ok(match value {
        Msgpack::Nil => Value::Null,
        Msgpack::Boolean(flag) => Value::Bool(*flag),
        Msgpack::Integer(integer) => match (integer.as_u64(), integer.as_i64()) {
            (Some(unsigned), _) => Value::from(unsigned),
            (_, Some(signed)) => Value::from(signed),
            _ => unreachable!("a msgpack integer fits u64 or i64"),
        },
        Msgpack::F32(float) => number(f64::from(*float), path)?,
        Msgpack::F64(float) => number(*float, path)?,
        Msgpack::String(text) => match text.as_str() {
            Some(text) => Value::String(text.to_owned()),
            None => return Err(format!("`{path}` is not UTF-8 text")),
        },
        Msgpack::Binary(_) => return Err(format!("`{path}` is bytes, which JSON can't hold")),
        Msgpack::Array(items) => Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| at(item, &Path::Item(path, index)))
                .collect::<Result<_, _>>()?,
        ),
        Msgpack::Map(entries) => {
            let mut fields = Map::new();
            for (key, value) in entries {
                let Some(key) = key.as_str() else {
                    return Err(format!(
                        "`{path}` has a map key that is not a string: {key}"
                    ));
                };
                fields.insert(key.to_owned(), at(value, &Path::Field(path, key))?);
            }
            Value::Object(fields)
        }
        Msgpack::Ext(kind, _) => return Err(format!("`{path}` is a msgpack extension ({kind})")),
    })
}

fn number(float: f64, path: &Path<'_>) -> Result<Value, String> {
    Number::from_f64(float)
        .map(Value::Number)
        .ok_or_else(|| format!("`{path}` is {float}, which JSON can't hold"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn json_round_trips_through_msgpack() {
        let value = json!({ "a": [1, -2, 1.5, "x", null, true], "b": { "c": u64::MAX } });
        assert_eq!(to_json(&to_msgpack(&value)).unwrap(), value);
    }

    #[test]
    fn values_json_cannot_hold_name_their_path() {
        let bytes = Msgpack::Map(vec![(
            "data".into(),
            Msgpack::Array(vec![Msgpack::Binary(vec![1])]),
        )]);
        assert_eq!(
            to_json(&bytes).unwrap_err(),
            "`$.data[0]` is bytes, which JSON can't hold"
        );
        let key = Msgpack::Map(vec![(Msgpack::from(1), Msgpack::Nil)]);
        assert!(to_json(&key).unwrap_err().contains("not a string"));
        assert!(to_json(&Msgpack::F64(f64::NAN)).is_err());
    }

    #[test]
    fn deep_error_paths_keep_all_segments_in_order() {
        let depth = 256;
        let value = (0..depth).fold(Msgpack::Binary(vec![1]), |leaf, _| {
            Msgpack::Map(vec![("child".into(), Msgpack::Array(vec![leaf]))])
        });
        assert_eq!(
            to_json(&value).unwrap_err(),
            format!(
                "`${}` is bytes, which JSON can't hold",
                ".child[0]".repeat(depth)
            )
        );
    }

    #[test]
    fn nested_errors_name_the_failing_sibling_only() {
        let value = Msgpack::Map(vec![
            ("earlier".into(), Msgpack::Array(vec![Msgpack::Nil])),
            (
                "items".into(),
                Msgpack::Array(vec![
                    Msgpack::Nil,
                    Msgpack::Map(vec![("bad".into(), Msgpack::Binary(vec![1]))]),
                ]),
            ),
        ]);
        assert_eq!(
            to_json(&value).unwrap_err(),
            "`$.items[1].bad` is bytes, which JSON can't hold"
        );
    }
}
