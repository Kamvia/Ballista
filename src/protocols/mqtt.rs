// File: ballista/src/protocols/mqtt.rs
use std::collections::HashSet;
use std::time::Duration;

use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, TlsConfiguration, Transport};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{MqttConfig, tls_client_config};
use crate::config::secret_from_env;
use crate::error::{Error, Result, sanitize};
use crate::vu::Vu;

const INBOX: usize = 1024;
const MAX_TOPIC: usize = 512;
const MAX_PAYLOAD: usize = 1024 * 1024;

enum Note {
    Ack,
    Message { topic: String, payload: Vec<u8> },
    Down(String),
}

/// One MQTT connection per virtual user, like a real device.
pub struct MqttSession {
    client: AsyncClient,
    notes: mpsc::Receiver<Note>,
    subscribed: HashSet<String>,
    timeout: Duration,
    task: JoinHandle<()>,
}

impl Drop for MqttSession {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn qos_of(q: u8) -> Result<QoS> {
    match q {
        0 => Ok(QoS::AtMostOnce),
        1 => Ok(QoS::AtLeastOnce),
        2 => Ok(QoS::ExactlyOnce),
        _ => Err(Error::check("mqtt qos must be 0, 1 or 2")),
    }
}

fn check_topic(t: &str) -> Result<()> {
    if t.is_empty() || t.len() > MAX_TOPIC || t.contains('\0') {
        return Err(Error::check("invalid mqtt topic"));
    }
    Ok(())
}

impl MqttSession {
    async fn connect(cfg: &MqttConfig, vu_id: u64) -> Result<Self> {
        let suffix: u32 = rand::random();
        let client_id = format!("{}-{vu_id}-{suffix:08x}", cfg.client_id_prefix);
        let mut opts = MqttOptions::new(client_id, cfg.host.clone(), cfg.port);
        opts.set_keep_alive(cfg.keep_alive.max(Duration::from_secs(5)));
        opts.set_clean_session(true);
        opts.set_max_packet_size(MAX_PAYLOAD + 4096, MAX_PAYLOAD + 4096);
        if let Some(user) = &cfg.username {
            let pass = cfg.password_env.as_deref().map(secret_from_env).transpose()?.unwrap_or_default();
            opts.set_credentials(user.clone(), pass);
        }
        if cfg.tls {
            let tls = tls_client_config(cfg.ca_cert.as_deref(), None)?;
            opts.set_transport(Transport::Tls(TlsConfiguration::Rustls(std::sync::Arc::new(tls))));
        }
        let (client, mut eventloop) = AsyncClient::new(opts, 64);
        let (tx, mut notes) = mpsc::channel(INBOX);
        let task = tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(Event::Incoming(Packet::ConnAck(_))) => {
                        let _ = tx.try_send(Note::Ack);
                    }
                    Ok(Event::Incoming(Packet::PubAck(_) | Packet::PubComp(_) | Packet::SubAck(_))) => {
                        let _ = tx.try_send(Note::Ack);
                    }
                    Ok(Event::Incoming(Packet::Publish(p))) => {
                        let _ = tx.try_send(Note::Message { topic: p.topic, payload: p.payload.to_vec() });
                    }
                    Ok(_) => {}
                    Err(e) => {
                        let _ = tx.send(Note::Down(e.to_string())).await;
                        return;
                    }
                }
            }
        });
        let mut s = Self { client, notes: mpsc::channel(1).1, subscribed: HashSet::new(), timeout: cfg.timeout, task };
        match tokio::time::timeout(cfg.timeout, notes.recv()).await {
            Ok(Some(Note::Ack)) => {
                s.notes = notes;
                Ok(s)
            }
            Ok(Some(Note::Down(e))) => Err(Error::check(format!("mqtt connect: {}", sanitize(&e, 200)))),
            _ => Err(Error::check("mqtt connect: timeout")),
        }
    }

    async fn wait_ack(&mut self) -> std::result::Result<(), String> {
        loop {
            match tokio::time::timeout(self.timeout, self.notes.recv()).await {
                Ok(Some(Note::Ack)) => return Ok(()),
                Ok(Some(Note::Message { .. })) => {}
                Ok(Some(Note::Down(e))) => return Err(e),
                Ok(None) => return Err("connection closed".into()),
                Err(_) => return Err("ack timeout".into()),
            }
        }
    }

    async fn wait_message(&mut self, topic: &str) -> std::result::Result<Vec<u8>, String> {
        loop {
            match tokio::time::timeout(self.timeout, self.notes.recv()).await {
                Ok(Some(Note::Message { topic: t, payload })) if t == topic => return Ok(payload),
                Ok(Some(Note::Message { .. } | Note::Ack)) => {}
                Ok(Some(Note::Down(e))) => return Err(e),
                Ok(None) => return Err("connection closed".into()),
                Err(_) => return Err("reply timeout".into()),
            }
        }
    }
}

impl Vu {
    async fn mqtt_session(&mut self) -> Result<&mut MqttSession> {
        if self.mqtt.is_none() {
            let cfg = self.ctx.protocols.mqtt.clone().ok_or_else(|| Error::check("mqtt is not configured in [protocols.mqtt]"))?;
            let id = self.id();
            let start = std::time::Instant::now();
            let key = crate::metrics::Key::request(self.scenario_arc(), "MQTT-CONNECT", &cfg.host);
            match MqttSession::connect(&cfg, id).await {
                Ok(s) => {
                    self.record(&key, start.elapsed(), true, None, (0, 0), None);
                    self.mqtt = Some(s);
                }
                Err(e) => {
                    let msg = e.to_string();
                    self.record(&key, start.elapsed(), false, None, (0, 0), Some(&msg));
                    return Err(Error::Check(msg));
                }
            }
        }
        self.mqtt.as_mut().ok_or_else(|| Error::check("mqtt session unavailable"))
    }

    /// Publishes a message. For QoS 1 and 2 the broker acknowledgement is included in the latency.
    pub async fn mqtt_publish(&mut self, topic: &str, payload: &[u8], qos: u8, retain: bool, name: &str) -> Result<()> {
        check_topic(topic)?;
        if payload.len() > MAX_PAYLOAD {
            return Err(Error::check("mqtt payload exceeds 1 MiB"));
        }
        let q = qos_of(qos)?;
        let data = payload.to_vec();
        let topic = topic.to_owned();
        let session = self.mqtt_session().await?;
        let client = session.client.clone();
        let fut = async {
            client.publish(topic, q, retain, data).await.map_err(|e| e.to_string())?;
            if q != QoS::AtMostOnce {
                session.wait_ack().await?;
            }
            Ok::<_, String>(())
        };
        let start = std::time::Instant::now();
        let res = fut.await;
        let key = crate::metrics::Key::request(self.scenario_arc(), "MQTT", name);
        self.record_outcome(&key, start.elapsed(), res)
    }

    /// Publishes to `topic` and waits for the next message on `reply_topic`; the round trip is recorded.
    pub async fn mqtt_request(&mut self, topic: &str, payload: &[u8], reply_topic: &str, qos: u8, name: &str) -> Result<Vec<u8>> {
        check_topic(topic)?;
        check_topic(reply_topic)?;
        let q = qos_of(qos)?;
        let data = payload.to_vec();
        let (topic, reply) = (topic.to_owned(), reply_topic.to_owned());
        let session = self.mqtt_session().await?;
        if !session.subscribed.contains(&reply) {
            session.client.subscribe(reply.clone(), q).await.map_err(|e| Error::check(format!("mqtt subscribe: {e}")))?;
            session.wait_ack().await.map_err(|e| Error::check(format!("mqtt subscribe: {e}")))?;
            session.subscribed.insert(reply.clone());
        }
        let client = session.client.clone();
        let start = std::time::Instant::now();
        let res = async {
            client.publish(topic, q, false, data).await.map_err(|e| e.to_string())?;
            session.wait_message(&reply).await
        }
        .await;
        let key = crate::metrics::Key::request(self.scenario_arc(), "MQTT", name);
        self.record_outcome(&key, start.elapsed(), res)
    }
}
