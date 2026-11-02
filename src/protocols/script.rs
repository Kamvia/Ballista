// File: ballista/src/protocols/script.rs
use std::collections::BTreeMap;
use std::time::Duration;

use rhai::{Dynamic, Map};

use super::net::TcpRead;
use crate::error::{Error, Result};
use crate::vu::Vu;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Non-HTTP request fields of a script request map.
#[derive(Debug, Default)]
pub struct ProtoRequest {
    pub grpc: Option<String>,
    pub message: Option<serde_json::Value>,
    pub metadata: BTreeMap<String, String>,
    pub mqtt: Option<String>,
    pub payload: Option<String>,
    pub qos: Option<u8>,
    pub retain: bool,
    pub reply: Option<String>,
    pub kafka: Option<String>,
    pub key: Option<String>,
    pub value: Option<String>,
    pub partition: Option<i32>,
    pub sql: Option<String>,
    pub params: Vec<serde_json::Value>,
    pub tcp: Option<String>,
    pub udp: Option<String>,
    pub send: Option<String>,
    pub read: Option<String>,
    pub read_bytes: Option<usize>,
    pub read_until: Option<String>,
    pub name: Option<String>,
    pub timeout: Option<Duration>,
    pub max_messages: Option<usize>,
}

impl ProtoRequest {
    pub fn is_set(&self) -> bool {
        self.grpc.is_some() || self.mqtt.is_some() || self.kafka.is_some() || self.sql.is_some() || self.tcp.is_some() || self.udp.is_some()
    }
}

fn ok_map() -> Map {
    let mut m = Map::new();
    m.insert("ok".into(), Dynamic::TRUE);
    m
}

#[cfg_attr(not(any(feature = "grpc", feature = "sql")), allow(dead_code))]
fn json_dynamic(v: &serde_json::Value) -> Dynamic {
    rhai::serde::to_dynamic(v).unwrap_or(Dynamic::UNIT)
}

fn text(bytes: &[u8]) -> Dynamic {
    Dynamic::from(String::from_utf8_lossy(bytes).into_owned())
}

/// Executes a protocol request described in a script map. Returns `None` when the map is an HTTP request.
pub async fn dispatch(vu: &mut Vu, r: ProtoRequest) -> Result<Option<Dynamic>> {
    let kinds = [r.grpc.is_some(), r.mqtt.is_some(), r.kafka.is_some(), r.sql.is_some(), r.tcp.is_some(), r.udp.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if kinds == 0 {
        return Ok(None);
    }
    if kinds > 1 {
        return Err(Error::Script("request may set only one of grpc, mqtt, kafka, sql, tcp, udp".into()));
    }
    let timeout = r.timeout.unwrap_or(DEFAULT_TIMEOUT);
    let mut m = ok_map();

    if let Some(addr) = &r.tcp {
        let read = match (r.read.as_deref(), r.read_bytes, &r.read_until) {
            (_, Some(n), _) => TcpRead::Bytes(n),
            (_, None, Some(d)) => TcpRead::Until(d.as_bytes().to_vec()),
            (Some("none"), _, _) => TcpRead::None,
            (Some("close"), _, _) => TcpRead::Close,
            (None | Some("first"), _, _) => TcpRead::First,
            (Some(_), _, _) => return Err(Error::Script("tcp read must be none, first or close".into())),
        };
        let name = r.name.as_deref().unwrap_or(addr);
        let out = vu.tcp_exchange(addr, r.send.as_deref().unwrap_or("").as_bytes(), read, timeout, name).await?;
        m.insert("body".into(), text(&out));
        return Ok(Some(Dynamic::from_map(m)));
    }
    if let Some(addr) = &r.udp {
        let name = r.name.as_deref().unwrap_or(addr);
        let expect = r.read.as_deref() != Some("none");
        let out = vu.udp_exchange(addr, r.send.as_deref().unwrap_or("").as_bytes(), expect, timeout, name).await?;
        m.insert("body".into(), text(&out));
        return Ok(Some(Dynamic::from_map(m)));
    }
    if let Some(method) = &r.grpc {
        return grpc(vu, method, &r, m).await;
    }
    if let Some(topic) = &r.mqtt {
        return mqtt(vu, topic, &r, m).await;
    }
    if let Some(topic) = &r.kafka {
        return kafka(vu, topic, &r, m).await;
    }
    if let Some(sql) = &r.sql {
        return sql_query(vu, sql, &r, m).await;
    }
    Ok(None)
}

#[cfg(feature = "grpc")]
async fn grpc(vu: &mut Vu, method: &str, r: &ProtoRequest, mut m: Map) -> Result<Option<Dynamic>> {
    use crate::protocols::grpc::GrpcInput;
    let msg = r.message.clone().unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    let input = match &msg {
        serde_json::Value::Array(items) => GrpcInput::Many(items),
        one => GrpcInput::One(one),
    };
    let max = r.max_messages.unwrap_or(1000);
    let out = vu.grpc_call(method, input, &r.metadata, r.name.as_deref().unwrap_or(method), max).await?;
    if let Some(first) = out.first() {
        m.insert("json".into(), json_dynamic(first));
    }
    let arr: rhai::Array = out.iter().map(json_dynamic).collect();
    m.insert("count".into(), Dynamic::from_int(arr.len() as rhai::INT));
    m.insert("messages".into(), Dynamic::from_array(arr));
    Ok(Some(Dynamic::from_map(m)))
}

#[cfg(not(feature = "grpc"))]
async fn grpc(_vu: &mut Vu, _method: &str, _r: &ProtoRequest, _m: Map) -> Result<Option<Dynamic>> {
    Err(Error::Script("this binary was built without the 'grpc' feature".into()))
}

#[cfg(feature = "mqtt")]
async fn mqtt(vu: &mut Vu, topic: &str, r: &ProtoRequest, mut m: Map) -> Result<Option<Dynamic>> {
    let payload = r.payload.as_deref().unwrap_or("").as_bytes();
    let qos = r.qos.unwrap_or(0);
    let name = r.name.as_deref().unwrap_or(topic);
    match &r.reply {
        Some(reply) => {
            let out = vu.mqtt_request(topic, payload, reply, qos, name).await?;
            m.insert("body".into(), text(&out));
        }
        None => vu.mqtt_publish(topic, payload, qos, r.retain, name).await?,
    }
    Ok(Some(Dynamic::from_map(m)))
}

#[cfg(not(feature = "mqtt"))]
async fn mqtt(_vu: &mut Vu, _topic: &str, _r: &ProtoRequest, _m: Map) -> Result<Option<Dynamic>> {
    Err(Error::Script("this binary was built without the 'mqtt' feature".into()))
}

#[cfg(feature = "kafka")]
async fn kafka(vu: &mut Vu, topic: &str, r: &ProtoRequest, mut m: Map) -> Result<Option<Dynamic>> {
    let value = r.value.as_deref().or(r.payload.as_deref()).unwrap_or("").as_bytes();
    let key = r.key.as_deref().map(str::as_bytes);
    let offset = vu.kafka_produce(topic, r.partition.unwrap_or(0), key, value, r.name.as_deref().unwrap_or(topic)).await?;
    m.insert("offset".into(), Dynamic::from_int(offset as rhai::INT));
    Ok(Some(Dynamic::from_map(m)))
}

#[cfg(not(feature = "kafka"))]
async fn kafka(_vu: &mut Vu, _topic: &str, _r: &ProtoRequest, _m: Map) -> Result<Option<Dynamic>> {
    Err(Error::Script("this binary was built without the 'kafka' feature".into()))
}

#[cfg(feature = "sql")]
async fn sql_query(vu: &mut Vu, sql: &str, r: &ProtoRequest, mut m: Map) -> Result<Option<Dynamic>> {
    let rows = vu.sql_query(sql, &r.params, r.name.as_deref().unwrap_or("query")).await?;
    let arr: rhai::Array = rows.iter().map(|row| json_dynamic(&serde_json::Value::Object(row.clone()))).collect();
    m.insert("count".into(), Dynamic::from_int(arr.len() as rhai::INT));
    m.insert("rows".into(), Dynamic::from_array(arr));
    Ok(Some(Dynamic::from_map(m)))
}

#[cfg(not(feature = "sql"))]
async fn sql_query(_vu: &mut Vu, _sql: &str, _r: &ProtoRequest, _m: Map) -> Result<Option<Dynamic>> {
    Err(Error::Script("this binary was built without the 'sql' feature".into()))
}
