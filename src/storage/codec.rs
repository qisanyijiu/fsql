use crate::value::{Point, Value};
use crate::{Error, Result};

/// - 将运行时 `Value` 编码为可写入数据库文件的紧凑标记字符串。
/// - Encodes a runtime `Value` into the compact tagged string stored in the database file.
/// - 输入值应当可序列化；向量和点类型会按有限数值位模式逐项编码。
/// - The input value should be serializable; vector and point variants are encoded item-by-item from their finite numeric bit patterns.
/// - 返回带类型前缀的文本表示，不修改原始值且不返回错误。
/// - Returns a tagged textual representation, does not mutate the source value, and does not return errors.
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

/// - 将数据库文件中的标记值字符串解析回运行时 `Value`。
/// - Parses a tagged value string from the database file back into a runtime `Value`.
/// - 输入必须包含受支持的类型前缀，并满足各类型对十六进制、布尔位和分隔符的约束。
/// - The input must use a supported type tag and satisfy each variant's hex, boolean, and delimiter constraints.
/// - 返回解析后的值；遇到未知类型或损坏编码时返回执行错误。
/// - Returns the parsed value; yields an execution error for unknown kinds or malformed encodings.
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

/// - 将向量载荷字段解析为 `Value::Vector`。
/// - Parses a vector payload into `Value::Vector`.
/// - 载荷可为空表示空向量，否则每个分量都必须是逗号分隔的 8 位十六进制 `f32` 位模式。
/// - The payload may be empty for an empty vector; otherwise each component must be an 8-digit hex `f32` bit pattern separated by commas.
/// - 返回向量值；任一分量无效时返回执行错误。
/// - Returns the vector value; yields an execution error when any component is invalid.
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

/// - 将点坐标载荷解析为 `Value::Point`。
/// - Parses a point payload into `Value::Point`.
/// - 载荷必须包含以逗号分隔的经度和纬度十六进制 `f64` 位模式。
/// - The payload must contain comma-separated longitude and latitude hex `f64` bit patterns.
/// - 返回点值；经纬度缺失或损坏时返回执行错误。
/// - Returns the point value; yields an execution error when either coordinate is missing or malformed.
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

/// - 将 UTF-8 字符串编码为数据库文件使用的十六进制文本。
/// - Encodes a UTF-8 string into the hexadecimal text used by the database file.
/// - 输入按字节逐个转换，不做转义压缩或规范化。
/// - The input is converted byte-by-byte without escaping, compression, or normalization.
/// - 返回纯十六进制字符串，不修改输入也不返回错误。
/// - Returns a plain hexadecimal string without mutating the input and without returning errors.
pub(crate) fn encode_string(input: &str) -> String {
    input
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// - 将十六进制文本恢复为 UTF-8 字符串。
/// - Decodes hexadecimal text back into a UTF-8 string.
/// - 输入长度必须为偶数，且每个字节片段都必须是有效十六进制并组成合法 UTF-8。
/// - The input length must be even, and each byte pair must be valid hex that forms legal UTF-8.
/// - 返回解码后的字符串；长度、十六进制或 UTF-8 非法时返回执行错误。
/// - Returns the decoded string; yields an execution error for invalid length, hex, or UTF-8.
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
    /// - 验证字符串和值编码在往返后保持原样。
    /// - Verifies string and value encodings survive round-trips unchanged.
    /// - 场景覆盖所有支持的值类型与空向量边界。
    /// - The scenario covers every supported value variant plus the empty-vector edge case.
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
    /// - 验证损坏的值编码和字符串编码都会返回错误。
    /// - Verifies malformed value and string encodings are rejected with errors.
    /// - 场景覆盖非法标签、坏十六进制、坏坐标和坏 UTF-8。
    /// - The scenario covers invalid tags, bad hex, broken coordinates, and invalid UTF-8.
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
