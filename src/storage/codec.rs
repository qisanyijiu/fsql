use crate::value::{Point, Value};
use crate::{Error, Result};

pub(crate) fn encode_value(value: &Value) -> String {
    match value {
        Value::Null => "N".into(),
        Value::Integer(value) => format!("I:{value}"),
        Value::Float(value) => format!("F:{:016x}", value.to_bits()),
        Value::Boolean(value) => format!("B:{}", if *value { 1 } else { 0 }),
        Value::Text(value) => format!("T:{}", encode_string(value)),
        Value::Vector(values) => {
            let values = values
                .iter()
                .map(|value| format!("{:08x}", value.to_bits()))
                .collect::<Vec<_>>()
                .join(",");
            format!("V:{values}")
        }
        Value::Point(point) => format!(
            "P:{:016x},{:016x}",
            point.lon.to_bits(),
            point.lat.to_bits()
        ),
    }
}

pub(crate) fn decode_value(input: &str) -> Result<Value> {
    let (kind, body) = input
        .split_once(':')
        .map_or((input, ""), |(kind, body)| (kind, body));
    match kind {
        "N" => Ok(Value::Null),
        "I" => body
            .parse::<i64>()
            .map(Value::Integer)
            .map_err(|_| Error::Execution("invalid integer in database file".into())),
        "F" => Ok(Value::Float(f64::from_bits(
            u64::from_str_radix(body, 16)
                .map_err(|_| Error::Execution("invalid float in database file".into()))?,
        ))),
        "B" => match body {
            "0" => Ok(Value::Boolean(false)),
            "1" => Ok(Value::Boolean(true)),
            _ => Err(Error::Execution("invalid boolean in database file".into())),
        },
        "T" => Ok(Value::Text(decode_string(body)?)),
        "V" => decode_vector(body),
        "P" => decode_point(body),
        _ => Err(Error::Execution(
            "unknown value kind in database file".into(),
        )),
    }
}

fn decode_vector(body: &str) -> Result<Value> {
    let values = if body.is_empty() {
        Vec::new()
    } else {
        body.split(',')
            .map(|part| {
                u32::from_str_radix(part, 16)
                    .map(f32::from_bits)
                    .map_err(|_| Error::Execution("invalid vector in database file".into()))
            })
            .collect::<Result<Vec<_>>>()?
    };
    Ok(Value::Vector(values))
}

fn decode_point(body: &str) -> Result<Value> {
    let (lon, lat) = body
        .split_once(',')
        .ok_or_else(|| Error::Execution("invalid point in database file".into()))?;
    Ok(Value::Point(Point {
        lon: f64::from_bits(
            u64::from_str_radix(lon, 16)
                .map_err(|_| Error::Execution("invalid point lon".into()))?,
        ),
        lat: f64::from_bits(
            u64::from_str_radix(lat, 16)
                .map_err(|_| Error::Execution("invalid point lat".into()))?,
        ),
    }))
}

pub(crate) fn encode_string(input: &str) -> String {
    input
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn decode_string(input: &str) -> Result<String> {
    if !input.len().is_multiple_of(2) {
        return Err(Error::Execution("invalid hex string length".into()));
    }
    let mut bytes = Vec::with_capacity(input.len() / 2);
    for chunk in input.as_bytes().chunks(2) {
        let chunk = std::str::from_utf8(chunk)
            .map_err(|_| Error::Execution("invalid hex string".into()))?;
        bytes.push(
            u8::from_str_radix(chunk, 16)
                .map_err(|_| Error::Execution("invalid hex string".into()))?,
        );
    }
    String::from_utf8(bytes).map_err(|_| Error::Execution("invalid utf-8 string".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_strings_and_values() {
        assert_eq!(decode_string(&encode_string("hello")).unwrap(), "hello");
        let values = [
            Value::Null,
            Value::Integer(-7),
            Value::Float(1.25),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Text("abc".into()),
            Value::Vector(vec![1.0, 2.0]),
            Value::Vector(Vec::new()),
            Value::Point(Point { lon: 1.0, lat: 2.0 }),
        ];
        for value in values {
            assert_eq!(decode_value(&encode_value(&value)).unwrap(), value);
        }
    }

    #[test]
    fn rejects_invalid_encoded_values() {
        let invalid = [
            "I:no",
            "F:no",
            "B:2",
            "T:a",
            "V:no",
            "P:1",
            "P:no,0000000000000000",
            "P:0000000000000000,no",
            "X:1",
        ];
        for value in invalid {
            assert!(decode_value(value).is_err(), "{value}");
        }
        assert!(decode_string("a").is_err());
        assert!(decode_string("zz").is_err());
        assert!(decode_string("ff").is_err());
        assert!(decode_string("€a").is_err());
    }
}
