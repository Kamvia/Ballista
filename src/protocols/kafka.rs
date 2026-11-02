// File: ballista/src/protocols/kafka.rs
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use rskafka::client::partition::{Compression, PartitionClient, UnknownTopicHandling};
use rskafka::client::{Client, ClientBuilder};
use rskafka::record::Record;
use tokio::sync::Mutex;

use super::{KafkaConfig, tls_client_config};
use crate::error::{Error, Result, sanitize};
use crate::vu::Vu;

const MAX_RECORD: usize = 4 * 1024 * 1024;
const MAX_PARTITION_CLIENTS: usize = 4096;

/// Shared Kafka producer with cached partition clients.
pub struct KafkaClient {
    client: Client,
    partitions: Mutex<HashMap<(String, i32), Arc<PartitionClient>>>,
    timeout: std::time::Duration,
}

impl KafkaClient {
    pub async fn connect(cfg: &KafkaConfig) -> Result<Self> {
        let mut b = ClientBuilder::new(cfg.brokers.clone());
        if cfg.tls {
            b = b.tls_config(Arc::new(tls_client_config(cfg.ca_cert.as_deref(), None)?));
        }
        let client = tokio::time::timeout(cfg.timeout, b.build())
            .await
            .map_err(|_| Error::config("kafka bootstrap timeout"))?
            .map_err(|e| Error::config(format!("kafka bootstrap: {}", sanitize(&e.to_string(), 200))))?;
        Ok(Self { client, partitions: Mutex::new(HashMap::new()), timeout: cfg.timeout })
    }

    async fn partition(&self, topic: &str, partition: i32) -> std::result::Result<Arc<PartitionClient>, String> {
        let mut map = self.partitions.lock().await;
        if let Some(pc) = map.get(&(topic.to_owned(), partition)) {
            return Ok(Arc::clone(pc));
        }
        if map.len() >= MAX_PARTITION_CLIENTS {
            return Err("too many distinct kafka partitions".into());
        }
        let pc = Arc::new(
            self.client.partition_client(topic.to_owned(), partition, UnknownTopicHandling::Error).await.map_err(|e| e.to_string())?,
        );
        map.insert((topic.to_owned(), partition), Arc::clone(&pc));
        Ok(pc)
    }
}

impl Vu {
    /// Produces one record and waits for the broker acknowledgement; returns the offset.
    pub async fn kafka_produce(&self, topic: &str, partition: i32, key: Option<&[u8]>, value: &[u8], name: &str) -> Result<i64> {
        if topic.is_empty() || topic.len() > 249 || !topic.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) {
            return Err(Error::check("invalid kafka topic name"));
        }
        if value.len() + key.map_or(0, <[u8]>::len) > MAX_RECORD || partition < 0 {
            return Err(Error::check("kafka record exceeds 4 MiB or partition is negative"));
        }
        let client = self.ctx.protocols.kafka.as_ref().ok_or_else(|| Error::check("kafka is not configured in [protocols.kafka]"))?;
        let record =
            Record { key: key.map(<[u8]>::to_vec), value: Some(value.to_vec()), headers: BTreeMap::new(), timestamp: chrono::Utc::now() };
        self.measure("KAFKA", name, async {
            let fut = async {
                let pc = client.partition(topic, partition).await?;
                let offsets = pc.produce(vec![record], Compression::NoCompression).await.map_err(|e| e.to_string())?;
                offsets.first().copied().ok_or_else(|| "no offset returned".to_owned())
            };
            tokio::time::timeout(client.timeout, fut).await.map_err(|_| "timeout".to_owned())?
        })
        .await
    }
}
