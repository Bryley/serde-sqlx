use deserializers::PgRowDeserializer;
use serde::de::Error;
use serde::de::{value::Error as DeError, Deserialize};

use sqlx::postgres::{PgRow, PgValueRef};

mod deserializers;
mod map_access;
mod seq_access;
mod json;

/// Convenience function: deserialize a PgRow into any T that implements Deserialize
pub fn from_pg_row<T>(row: PgRow) -> Result<T, DeError>
where
    T: for<'de> Deserialize<'de>,
{
    let deserializer = PgRowDeserializer::new(&row);
    T::deserialize(deserializer)
}

fn decode_raw_pg<'a, T>(raw_value: PgValueRef<'a>) -> Result<T, DeError>
where
    T: sqlx::Decode<'a, sqlx::Postgres>,
{
    T::decode(raw_value).map_err(|err| {
        DeError::custom(format!(
            "Failed to decode {} value: {:?}",
            std::any::type_name::<T>(),
            err,
        ))
    })
}

