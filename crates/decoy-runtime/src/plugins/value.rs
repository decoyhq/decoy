//! JSON ⇄ msgpack for values crossing to a plugin. Cases and templates work on JSON; a plugin also
//! sees and returns wire bytes, as msgpack `bin`.

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

/// The JSON a plugin's value stands for. Bytes, non-string map keys, non-UTF-8 strings and
/// extension types have none; the error names where the first one is (`$.data`).
pub fn to_json(value: &Msgpack) -> Result<Value, String> {
    at(value, "$")
}

fn at(value: &Msgpack, path: &str) -> Result<Value, String> {
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
                .map(|(index, item)| at(item, &format!("{path}[{index}]")))
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
                fields.insert(key.to_owned(), at(value, &format!("{path}.{key}"))?);
            }
            Value::Object(fields)
        }
        Msgpack::Ext(kind, _) => return Err(format!("`{path}` is a msgpack extension ({kind})")),
    })
}

fn number(float: f64, path: &str) -> Result<Value, String> {
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
}
