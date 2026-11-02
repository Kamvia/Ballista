// File: ballista/src/protocols/grpc.rs
use std::collections::BTreeMap;
use std::str::FromStr;

use bytes::{Buf, BufMut};
use http::uri::PathAndQuery;
use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MethodDescriptor};
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::metadata::{MetadataKey, MetadataValue};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};

use super::GrpcConfig;
use crate::error::{Error, Result, sanitize};
use crate::vu::Vu;

const MAX_DESCRIPTOR: u64 = 32 * 1024 * 1024;
const MAX_MESSAGE: usize = 16 * 1024 * 1024;
const MAX_STREAM_MESSAGES: usize = 10_000;

/// Passes already-encoded protobuf bytes through tonic unchanged.
#[derive(Default, Clone)]
struct RawCodec;

#[derive(Default)]
struct RawEncoder;

#[derive(Default)]
struct RawDecoder;

impl Encoder for RawEncoder {
    type Item = Vec<u8>;
    type Error = tonic::Status;

    fn encode(&mut self, item: Self::Item, dst: &mut EncodeBuf<'_>) -> std::result::Result<(), Self::Error> {
        dst.put_slice(&item);
        Ok(())
    }
}

impl Decoder for RawDecoder {
    type Item = Vec<u8>;
    type Error = tonic::Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> std::result::Result<Option<Self::Item>, Self::Error> {
        let n = src.remaining();
        if n > MAX_MESSAGE {
            return Err(tonic::Status::resource_exhausted("response exceeds 16 MiB"));
        }
        Ok(Some(src.copy_to_bytes(n).to_vec()))
    }
}

impl Codec for RawCodec {
    type Encode = Vec<u8>;
    type Decode = Vec<u8>;
    type Encoder = RawEncoder;
    type Decoder = RawDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        RawEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        RawDecoder
    }
}

/// Dynamic gRPC client driven by a descriptor set: requests and responses are JSON.
pub struct GrpcClient {
    channel: Channel,
    pool: DescriptorPool,
}

fn read_limited(path: &std::path::Path, max: u64, what: &str) -> Result<Vec<u8>> {
    let meta = std::fs::metadata(path).map_err(|e| Error::config(format!("{what} {}: {e}", path.display())))?;
    if !meta.is_file() || meta.len() > max {
        return Err(Error::config(format!("{what} must be a regular file within size limits")));
    }
    Ok(std::fs::read(path)?)
}

impl GrpcClient {
    pub async fn connect(cfg: &GrpcConfig) -> Result<Self> {
        let pool = DescriptorPool::decode(read_limited(&cfg.descriptor_set, MAX_DESCRIPTOR, "descriptor_set")?.as_slice())
            .map_err(|e| Error::config(format!("invalid descriptor set: {e}")))?;
        let mut ep = Endpoint::from_str(&cfg.endpoint)
            .map_err(|e| Error::config(format!("grpc endpoint: {e}")))?
            .timeout(cfg.timeout)
            .connect_timeout(cfg.timeout)
            .tcp_nodelay(true);
        if cfg.endpoint.starts_with("https://") {
            let mut tls = ClientTlsConfig::new().with_webpki_roots();
            if let Some(ca) = &cfg.ca_cert {
                tls = tls.ca_certificate(Certificate::from_pem(read_limited(ca, 1 << 20, "grpc ca_cert")?));
            }
            if let (Some(c), Some(k)) = (&cfg.client_cert, &cfg.client_key) {
                tls = tls.identity(Identity::from_pem(
                    read_limited(c, 1 << 20, "grpc client_cert")?,
                    read_limited(k, 1 << 20, "grpc client_key")?,
                ));
            }
            if let Some(d) = &cfg.domain {
                tls = tls.domain_name(d.clone());
            }
            ep = ep.tls_config(tls).map_err(|e| Error::config(format!("grpc tls: {e}")))?;
        }
        Ok(Self { channel: ep.connect_lazy(), pool })
    }

    fn method(&self, full: &str) -> Result<MethodDescriptor> {
        let (svc, name) =
            full.trim_start_matches('/').split_once('/').ok_or_else(|| Error::check("grpc method must be 'package.Service/Method'"))?;
        let service =
            self.pool.get_service_by_name(svc).ok_or_else(|| Error::check(format!("unknown grpc service '{}'", sanitize(svc, 80))))?;
        let method = service
            .methods()
            .find(|m| m.name() == name)
            .ok_or_else(|| Error::check(format!("unknown grpc method '{}'", sanitize(name, 80))))?;
        Ok(method)
    }

    /// Performs any call shape. Streaming requests send all messages, then read up to `max` replies.
    async fn call(
        &self,
        method: &MethodDescriptor,
        mut inputs: Vec<Vec<u8>>,
        metadata: &BTreeMap<String, String>,
        max: usize,
    ) -> std::result::Result<Vec<Vec<u8>>, String> {
        let mut grpc = tonic::client::Grpc::new(self.channel.clone());
        grpc.ready().await.map_err(|e| format!("grpc transport: {e}"))?;
        let path =
            PathAndQuery::from_str(&format!("/{}/{}", method.parent_service().full_name(), method.name())).map_err(|e| e.to_string())?;
        let status = |s: tonic::Status| format!("grpc status {:?}: {}", s.code(), sanitize(s.message(), 200));
        match (method.is_client_streaming(), method.is_server_streaming()) {
            (false, false) => {
                let one = inputs.pop().ok_or("grpc request message missing")?;
                let resp = grpc.unary(with_metadata(one, metadata)?, path, RawCodec).await.map_err(status)?;
                Ok(vec![resp.into_inner()])
            }
            (false, true) => {
                let one = inputs.pop().ok_or("grpc request message missing")?;
                let resp = grpc.server_streaming(with_metadata(one, metadata)?, path, RawCodec).await.map_err(status)?;
                collect(resp.into_inner(), max).await.map_err(status)
            }
            (true, false) => {
                let stream = futures_util::stream::iter(inputs);
                let resp = grpc.client_streaming(with_metadata(stream, metadata)?, path, RawCodec).await.map_err(status)?;
                Ok(vec![resp.into_inner()])
            }
            (true, true) => {
                let stream = futures_util::stream::iter(inputs);
                let resp = grpc.streaming(with_metadata(stream, metadata)?, path, RawCodec).await.map_err(status)?;
                collect(resp.into_inner(), max).await.map_err(status)
            }
        }
    }
}

fn with_metadata<T>(body: T, metadata: &BTreeMap<String, String>) -> std::result::Result<tonic::Request<T>, String> {
    let mut req = tonic::Request::new(body);
    for (k, v) in metadata {
        let key = MetadataKey::from_bytes(k.to_ascii_lowercase().as_bytes()).map_err(|_| "invalid metadata key".to_owned())?;
        let val = MetadataValue::try_from(v.as_str()).map_err(|_| "invalid metadata value".to_owned())?;
        req.metadata_mut().insert(key, val);
    }
    Ok(req)
}

async fn collect(mut stream: tonic::codec::Streaming<Vec<u8>>, max: usize) -> std::result::Result<Vec<Vec<u8>>, tonic::Status> {
    let mut out = Vec::new();
    while out.len() < max {
        match stream.message().await? {
            Some(m) => out.push(m),
            None => break,
        }
    }
    Ok(out)
}

/// Request messages for a gRPC call: one message, or a sequence for client and bidirectional streams.
#[derive(Debug, Clone, Copy)]
pub enum GrpcInput<'a> {
    One(&'a serde_json::Value),
    Many(&'a [serde_json::Value]),
}

impl Vu {
    /// Unary gRPC call with a JSON request and JSON response, recorded as `GRPC name`.
    pub async fn grpc(
        &self,
        method_full: &str,
        message: &serde_json::Value,
        metadata: &BTreeMap<String, String>,
        name: &str,
    ) -> Result<serde_json::Value> {
        let mut out = self.grpc_call(method_full, GrpcInput::One(message), metadata, name, 1).await?;
        out.pop().ok_or_else(|| Error::check("grpc call returned no message"))
    }

    /// Any gRPC call shape (unary, server, client or bidirectional streaming) with JSON messages.
    /// The whole exchange is recorded as `GRPC name`; at most `max_messages` replies are read.
    pub async fn grpc_call(
        &self,
        method_full: &str,
        input: GrpcInput<'_>,
        metadata: &BTreeMap<String, String>,
        name: &str,
        max_messages: usize,
    ) -> Result<Vec<serde_json::Value>> {
        let client = self.ctx.protocols.grpc.as_ref().ok_or_else(|| Error::check("grpc is not configured in [protocols.grpc]"))?;
        let method = client.method(method_full)?;
        let messages: &[serde_json::Value] = match input {
            GrpcInput::One(m) => std::slice::from_ref(m),
            GrpcInput::Many(m) => m,
        };
        if messages.is_empty() || messages.len() > MAX_STREAM_MESSAGES {
            return Err(Error::check(format!("grpc calls take 1..{MAX_STREAM_MESSAGES} request messages")));
        }
        if messages.len() > 1 && !method.is_client_streaming() {
            return Err(Error::check("multiple request messages require a client or bidirectional streaming method"));
        }
        let mut encoded = Vec::with_capacity(messages.len());
        for m in messages {
            let input = DynamicMessage::deserialize(method.input(), m.clone())
                .map_err(|e| Error::check(format!("grpc request does not match schema: {}", sanitize(&e.to_string(), 200))))?;
            encoded.push(input.encode_to_vec());
        }
        let max = max_messages.clamp(1, MAX_STREAM_MESSAGES);
        let raw = self.measure("GRPC", name, client.call(&method, encoded, metadata, max)).await?;
        raw.iter()
            .map(|r| {
                let out = DynamicMessage::decode(method.output(), r.as_slice())
                    .map_err(|e| Error::check(format!("grpc response decode: {e}")))?;
                serde_json::to_value(&out).map_err(|e| Error::check(format!("grpc response json: {e}")))
            })
            .collect()
    }
}
