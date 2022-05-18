// Copyright 2022 Singularity Data
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Value encoding is an encoding format which converts the data into a binary form (not
//! memcomparable).

use std::ops::Deref;

use bytes::{Buf, BufMut};
use chrono::{Datelike, Timelike};
use itertools::Itertools;
use prost::Message;
use risingwave_pb::expr::StructValue as ProstStructValue;

use crate::error::{Result, RwError};
use crate::types::{
    DataType, Datum, Decimal, IntervalUnit, NaiveDateTimeWrapper, NaiveDateWrapper,
    NaiveTimeWrapper, OrderedF32, OrderedF64, ScalarImpl, ScalarRefImpl,
};

pub mod error;
use error::ValueEncodingError;

use crate::array::{StructRef, StructValue};

/// Serialize datum into cell bytes (Not order guarantee, used in value encoding).
pub fn serialize_cell(cell: &Datum) -> Result<Vec<u8>> {
    let mut buf: Vec<u8> = vec![];
    if let Some(datum) = cell {
        serialize_value(datum.as_scalar_ref_impl(), &mut buf)
    }
    Ok(buf)
}

/// Deserialize cell bytes into datum (Not order guarantee, used in value decoding).
pub fn deserialize_cell(mut data: impl Buf, ty: &DataType) -> Result<Datum> {
    if data.remaining() < 1 {
        return Ok(None);
    }
    deserialize_value(ty, &mut data)
}

fn serialize_value(value: ScalarRefImpl, mut buf: impl BufMut) {
    match value {
        ScalarRefImpl::Int16(v) => buf.put_i16_le(v),
        ScalarRefImpl::Int32(v) => buf.put_i32_le(v),
        ScalarRefImpl::Int64(v) => buf.put_i64_le(v),
        ScalarRefImpl::Float32(v) => buf.put_f32_le(v.into_inner()),
        ScalarRefImpl::Float64(v) => buf.put_f64_le(v.into_inner()),
        ScalarRefImpl::Utf8(v) => serialize_str(v.as_bytes(), buf),
        ScalarRefImpl::Bool(v) => buf.put_u8(v as u8),
        ScalarRefImpl::Decimal(v) => serialize_decimal(&v, buf),
        ScalarRefImpl::Interval(v) => serialize_interval(&v, buf),
        ScalarRefImpl::NaiveDate(v) => serialize_naivedate(v.0.num_days_from_ce(), buf),
        ScalarRefImpl::NaiveDateTime(v) => {
            serialize_naivedatetime(v.0.timestamp(), v.0.timestamp_subsec_nanos(), buf)
        }
        ScalarRefImpl::NaiveTime(v) => {
            serialize_naivetime(v.0.num_seconds_from_midnight(), v.0.nanosecond(), buf)
        }
        ScalarRefImpl::Struct(StructRef::ValueRef { val }) => {
            let bytes = val.to_protobuf_owned();
            buf.put_u32_le(bytes.len() as u32);
            buf.put_slice(bytes.as_slice());
        }
        _ => {
            panic!("Type is unable to be serialized.")
        }
    }
}

fn serialize_str(bytes: &[u8], mut buf: impl BufMut) {
    buf.put_u32_le(bytes.len() as u32);
    buf.put_slice(bytes);
}

fn serialize_interval(interval: &IntervalUnit, mut buf: impl BufMut) {
    buf.put_i32_le(interval.get_months());
    buf.put_i32_le(interval.get_days());
    buf.put_i64_le(interval.get_ms());
}

fn serialize_naivedate(days: i32, mut buf: impl BufMut) {
    buf.put_i32_le(days);
}

fn serialize_naivedatetime(secs: i64, nsecs: u32, mut buf: impl BufMut) {
    buf.put_i64_le(secs);
    buf.put_u32_le(nsecs);
}

fn serialize_naivetime(secs: u32, nano: u32, mut buf: impl BufMut) {
    buf.put_u32_le(secs);
    buf.put_u32_le(nano);
}

fn serialize_decimal(decimal: &Decimal, mut buf: impl BufMut) {
    buf.put_slice(&decimal.unordered_serialize());
}

fn deserialize_value(ty: &DataType, mut data: impl Buf) -> Result<Datum> {
    Ok(Some(match ty {
        DataType::Int16 => ScalarImpl::Int16(data.get_i16_le()),
        DataType::Int32 => ScalarImpl::Int32(data.get_i32_le()),
        DataType::Int64 => ScalarImpl::Int64(data.get_i64_le()),
        DataType::Float32 => ScalarImpl::Float32(OrderedF32::from(data.get_f32_le())),
        DataType::Float64 => ScalarImpl::Float64(OrderedF64::from(data.get_f64_le())),
        DataType::Varchar => ScalarImpl::Utf8(deserialize_str(data)?),
        DataType::Boolean => ScalarImpl::Bool(deserialize_bool(data)?),
        DataType::Decimal => ScalarImpl::Decimal(deserialize_decimal(data)?),
        DataType::Interval => ScalarImpl::Interval(deserialize_interval(data)?),
        DataType::Time => ScalarImpl::NaiveTime(deserialize_naivetime(data)?),
        DataType::Timestamp => ScalarImpl::NaiveDateTime(deserialize_naivedatetime(data)?),
        DataType::Timestampz => ScalarImpl::Int64(data.get_i64_le()),
        DataType::Date => ScalarImpl::NaiveDate(deserialize_naivedate(data)?),
        DataType::Struct { fields } => {
            ScalarImpl::Struct(deserialize_struct(fields.deref(), data)?)
        }
        _ => {
            panic!("Type is unable to be deserialized.")
        }
    }))
}

fn deserialize_struct(data_type: &[DataType], mut data: impl Buf) -> Result<StructValue> {
    let len = data.get_u32_le();
    let mut bytes = vec![0; len as usize];
    tracing::info!("{:?}", len);
    data.copy_to_slice(&mut bytes);
    let struct_value: ProstStructValue = Message::decode(bytes.as_slice())?;
    tracing::info!("{:?}", struct_value.body);
    let fields = struct_value
        .body
        .iter()
        .zip_eq(data_type.iter())
        .map(|(b, d)| deserialize_value(d, b.as_slice()))
        .collect::<Result<Vec<_>>>()?;
    Ok(StructValue::new(fields))
}

fn deserialize_str(mut data: impl Buf) -> Result<String> {
    let len = data.get_u32_le();
    let mut bytes = vec![0; len as usize];
    data.copy_to_slice(&mut bytes);
    Ok(String::from_utf8(bytes).map_err(ValueEncodingError::InvalidUtf8)?)
}

fn deserialize_bool(mut data: impl Buf) -> Result<bool> {
    match data.get_u8() {
        1 => Ok(true),
        0 => Ok(false),
        value => Err(RwError::from(ValueEncodingError::InvalidBoolEncoding(
            value,
        ))),
    }
}

fn deserialize_interval(mut data: impl Buf) -> Result<IntervalUnit> {
    let months = data.get_i32_le();
    let days = data.get_i32_le();
    let ms = data.get_i64_le();
    Ok(IntervalUnit::new(months, days, ms))
}

fn deserialize_naivetime(mut data: impl Buf) -> Result<NaiveTimeWrapper> {
    let secs = data.get_u32_le();
    let nano = data.get_u32_le();
    NaiveTimeWrapper::new_with_secs_nano_value_encoding(secs, nano)
}

fn deserialize_naivedatetime(mut data: impl Buf) -> Result<NaiveDateTimeWrapper> {
    let secs = data.get_i64_le();
    let nsecs = data.get_u32_le();
    NaiveDateTimeWrapper::new_with_secs_nsecs_value_encoding(secs, nsecs)
}

fn deserialize_naivedate(mut data: impl Buf) -> Result<NaiveDateWrapper> {
    let days = data.get_i32_le();
    NaiveDateWrapper::new_with_days_value_encoding(days)
}

fn deserialize_decimal(mut data: impl Buf) -> Result<Decimal> {
    let mut bytes = [0; 16];
    data.copy_to_slice(&mut bytes);
    Ok(Decimal::unordered_deserialize(bytes))
}
