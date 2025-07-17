
use crate::decode_raw_pg;
use crate::json::PgJson;
use crate::map_access::PgRowMapAccess;
use crate::seq_access::{PgArraySeqAccess, PgRowSeqAccess};
use serde::de::{value::Error as DeError, Deserializer, Visitor};
use serde::de::{Error as _, IntoDeserializer};
use serde::forward_to_deserialize_any;
use sqlx::postgres::{PgRow, PgValueRef};
use sqlx::{Row, TypeInfo, ValueRef};

#[derive(Clone, Copy)]
pub struct PgRowDeserializer<'a> {
    pub(crate) row: &'a PgRow,
    pub(crate) index: usize,
}

impl<'a> PgRowDeserializer<'a> {
    pub fn new(row: &'a PgRow) -> Self {
        PgRowDeserializer { row, index: 0 }
    }

    #[allow(unused)]
    pub fn is_json(&self) -> bool {
        self.row.try_get_raw(0).is_ok_and(|value|
            matches!(value.type_info().name(), "JSON" | "JSONB")
        )
    }
}

impl<'de, 'a> Deserializer<'de> for PgRowDeserializer<'a> {
    type Error = DeError;

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let raw_value = self.row.try_get_raw(0).map_err(DeError::custom)?;

        if raw_value.is_null() {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.row.columns().len() {
            0 => return visitor.visit_unit(),
            1 => {}
            _n => {
                return self.deserialize_seq(visitor);
            }
        };

        let raw_value = self.row.try_get_raw(self.index).map_err(DeError::custom)?;
        let type_info = raw_value.type_info();
        let type_name = type_info.name();

        if raw_value.is_null() {
            return visitor.visit_none();
        }

        // If this is a BOOL[], TEXT[], etc
        if type_name.ends_with("[]") {
            return self.deserialize_seq(visitor);
        }

        // Direct all "basic" types down to `PgValueDeserializer`
        let deserializer = PgValueDeserializer { value: raw_value };

        deserializer.deserialize_any(visitor)
    }

    /// We treat the row as a map (each column is a key/value pair)
    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_map(PgRowMapAccess {
            deserializer: self,
            num_cols: self.row.columns().len(),
        })
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let raw_value = self.row.try_get_raw(self.index).map_err(DeError::custom)?;
        let type_info = raw_value.type_info();
        let type_name = type_info.name();

        match type_name {
            "TEXT[]" | "VARCHAR[]" => {
                let seq_access = PgArraySeqAccess::<String>::new(raw_value)?;
                visitor.visit_seq(seq_access)
            }
            "INT4[]" => {
                let seq_access = PgArraySeqAccess::<i32>::new(raw_value)?;
                visitor.visit_seq(seq_access)
            }
            "JSON[]" | "JSONB[]" => {
                let seq_access = PgArraySeqAccess::<PgJson>::new(raw_value)?;
                visitor.visit_seq(seq_access)
            }
            "BOOL[]" => {
                let seq_access = PgArraySeqAccess::<bool>::new(raw_value)?;
                visitor.visit_seq(seq_access)
            }
            _ => {
                let seq_access = PgRowSeqAccess {
                    deserializer: self,
                    num_cols: self.row.columns().len(),
                };

                visitor.visit_seq(seq_access)
            }
        }
    }

    fn deserialize_tuple<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let raw_value = self.row.try_get_raw(self.index).map_err(DeError::custom)?;
        let type_info = raw_value.type_info();
        let type_name = type_info.name();

        if type_name == "JSON" || type_name == "JSONB" {
            let value = decode_raw_pg::<PgJson>(raw_value)
                .map_err(|err| DeError::custom(format!("Failed to decode JSON/JSONB: {err}")))?;

            if let serde_json::Value::Object(ref obj) = value.0 {
                if fields.len() == 1 {
                    // If there's only one expected field, check if the object already contains it.
                    if obj.contains_key(fields[0]) {
                        // If so, we can deserialize directly.
                        return value.into_deserializer().deserialize_any(visitor);
                    } else {
                        // Otherwise, wrap the object in a new map keyed by that field name.
                        let mut map = serde_json::Map::new();
                        map.insert(fields[0].to_owned(), value.0);
                        return map
                            .into_deserializer()
                            .deserialize_any(visitor)
                            .map_err(DeError::custom);
                    }
                } else {
                    // For multiple expected fields, ensure the JSON object already contains all of them.
                    if fields.iter().all(|&field| obj.contains_key(field)) {
                        return value.into_deserializer().deserialize_any(visitor);
                    } else {
                        return Err(DeError::custom(format!(
                            "JSON object missing expected keys: expected {:?}, found keys {:?}",
                            fields,
                            obj.keys().collect::<Vec<_>>()
                        )));
                    }
                }
            } else {
                // For non-object JSON values, delegate directly.
                return value.into_deserializer().deserialize_any(visitor);
            }
        }

        // Fallback for non-JSON types.
        self.deserialize_map(visitor)
    }

    // For other types, forward to deserialize_any.
    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string
        bytes byte_buf unit unit_struct
        tuple_struct enum identifier ignored_any
    }
}

/// An "inner" deserializer
#[derive(Clone)]
pub(crate) struct PgValueDeserializer<'a> {
    pub(crate) value: PgValueRef<'a>,
}

impl<'de, 'a> Deserializer<'de> for PgValueDeserializer<'a> {
    type Error = DeError;

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        if self.value.is_null() {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        if self.value.is_null() {
            return visitor.visit_none();
        }
        let type_info = self.value.type_info();

        let type_name = type_info.name();

        match type_name {
            "FLOAT4" => {
                let v = decode_raw_pg::<f32>(self.value)?;
                visitor.visit_f32(v)
            }
            "FLOAT8" => {
                let v = decode_raw_pg::<f64>(self.value)?;
                visitor.visit_f64(v)
            }
            "NUMERIC" => {
                let numeric = decode_raw_pg::<rust_decimal::Decimal>(self.value)?;

                let num: f64 = numeric
                    .try_into()
                    .map_err(|_| DeError::custom("Failed to parse Decimal as f64"))?;

                visitor.visit_f64(num)
            }
            "INT8" => {
                let v = decode_raw_pg::<i64>(self.value)?;
                visitor.visit_i64(v)
            }
            "INT4" => {
                let v = decode_raw_pg::<i32>(self.value)?;
                visitor.visit_i32(v)
            }
            "INT2" => {
                let v = decode_raw_pg::<i16>(self.value)?;
                visitor.visit_i16(v)
            }
            "BOOL" => {
                let v = decode_raw_pg::<bool>(self.value)?;
                visitor.visit_bool(v)
            }
            "DATE" => {
                let date = decode_raw_pg::<chrono::NaiveDate>(self.value)?;
                visitor.visit_string(date.to_string())
            }
            "TIME" | "TIMETZ" => {
                let time = decode_raw_pg::<chrono::NaiveTime>(self.value)?;
                visitor.visit_string(time.to_string())
            }
            "TIMESTAMP" | "TIMESTAMPTZ" => {
                let ts = decode_raw_pg::<chrono::DateTime<chrono::FixedOffset>>(self.value)?;
                visitor.visit_string(ts.to_rfc3339())
            }
            "UUID" => {
                let uuid = decode_raw_pg::<uuid::Uuid>(self.value)?;
                visitor.visit_string(uuid.to_string())
            }
            "BYTEA" => {
                let bytes = decode_raw_pg::<&[u8]>(self.value)?;
                visitor.visit_bytes(bytes)
            }
            "INTERVAL" => {
                let pg_interval = decode_raw_pg::<sqlx::postgres::types::PgInterval>(self.value)?;
                let secs = pg_interval.microseconds / 1_000_000;
                let nanos = (pg_interval.microseconds % 1_000_000) * 1000;
                let days_duration = chrono::Duration::days(pg_interval.days as i64);
                let duration = chrono::Duration::seconds(secs)
                    + chrono::Duration::nanoseconds(nanos)
                    + days_duration;
                visitor.visit_string(duration.to_string())
            }
            "CHAR" | "TEXT" => {
                let s = decode_raw_pg::<String>(self.value)?;
                visitor.visit_string(s)
            }
            "JSON" | "JSONB" => {
                let value = decode_raw_pg::<PgJson>(self.value)?;

                value.into_deserializer().deserialize_any(visitor)
            }
            _other => {
                let as_string = decode_raw_pg::<String>(self.value.clone())?;
                visitor.visit_string(as_string)
            }
        }
    }

    // For other types, forward to deserialize_any.
    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string
        bytes byte_buf unit unit_struct newtype_struct struct
        tuple_struct enum identifier ignored_any tuple seq map
    }
}
