//! Multi-engine routing for the UI.
//!
//! The UI owns one RPC client, but that client is backed by the home engine and
//! any SSH engines connected during this session. Workspace watches are merged
//! here; device-scoped calls are routed to the connected engine that owns the
//! requested device and fall back to the home engine's relay otherwise.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, Weak};

use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use zeron_rpc::{RpcClient, RpcError, RpcReply, RpcService, memory_client, methods};

use crate::state::EngineHandle;

const HOME_SOURCE: &str = "home";

#[derive(Default)]
struct Ownership {
    chats: HashMap<String, Owner>,
    spaces: HashMap<String, Owner>,
    devices: HashMap<String, Owner>,
}

#[derive(Clone)]
struct Owner {
    source: String,
    engine: EngineHandle,
}

/// The home engine plus all currently connected SSH engines.
///
/// The home engine is never removed. SSH engines are keyed by their engine
/// device id, which is stable for the lifetime of the engine process and is
/// also the id used by `targetDeviceId` throughout the UI.
pub struct EngineHub {
    home: EngineHandle,
    remotes: RwLock<HashMap<String, EngineHandle>>,
    ownership: RwLock<Ownership>,
    shutdown: AtomicBool,
}

impl EngineHub {
    pub(crate) fn new(home: EngineHandle) -> Arc<Self> {
        Arc::new(Self {
            home,
            remotes: RwLock::new(HashMap::new()),
            ownership: RwLock::new(Ownership::default()),
            shutdown: AtomicBool::new(false),
        })
    }

    pub(crate) fn router_client(hub: &Arc<Self>) -> RpcClient {
        memory_client(Arc::new(RouterService {
            hub: Arc::downgrade(hub),
        }))
    }

    /// Add an SSH engine. The return value is an older connection with the
    /// same device id, if any; callers should shut that handle down after the
    /// replacement has been installed.
    pub fn add_remote(&self, engine: EngineHandle) -> Option<EngineHandle> {
        let device_id = engine.engine_info().device_id.clone();
        self.clear_ownership_source(&device_id);
        self.remotes
            .write()
            .unwrap()
            .insert(device_id.clone(), engine)
    }

    /// Remove the SSH engine whose destination matches the saved target.
    pub fn remove_remote_destination(&self, destination: &str) -> Option<EngineHandle> {
        let device_id = self
            .remotes
            .read()
            .unwrap()
            .iter()
            .find_map(|(device_id, engine)| {
                (engine.remote_destination().as_deref() == Some(destination))
                    .then(|| device_id.clone())
            })?;
        self.clear_ownership_source(&device_id);
        self.remotes.write().unwrap().remove(&device_id)
    }

    fn clear_ownership_source(&self, source: &str) {
        let mut ownership = self.ownership.write().unwrap();
        ownership.chats.retain(|_, owner| owner.source != source);
        ownership.spaces.retain(|_, owner| owner.source != source);
        ownership.devices.retain(|_, owner| owner.source != source);
    }

    pub fn is_connected_destination(&self, destination: &str) -> bool {
        self.remotes
            .read()
            .unwrap()
            .values()
            .any(|engine| engine.remote_destination().as_deref() == Some(destination))
    }

    async fn engines(&self) -> Vec<(String, EngineHandle)> {
        let mut engines = vec![(HOME_SOURCE.to_string(), self.home.clone())];
        engines.extend(
            self.remotes
                .read()
                .unwrap()
                .iter()
                .map(|(device_id, engine)| (device_id.clone(), engine.clone())),
        );
        engines
    }

    async fn route(&self, params: &serde_json::Value) -> EngineHandle {
        // An explicit target is authoritative only for a connected hub engine.
        // If the device is merely date-synced, the home engine must relay it.
        if let Some(target_value) = params.get("targetDeviceId") {
            if let Some(target) = target_value.as_str()
                && let Some(engine) = self.remotes.read().unwrap().get(target).cloned()
            {
                return engine;
            }
            return self.home.clone();
        }

        let ownership = self.ownership.read().unwrap();
        if let Some(chat_id) = params.get("chatId").and_then(|v| v.as_str())
            && let Some(owner) = ownership.chats.get(chat_id)
        {
            return owner.engine.clone();
        }
        if let Some(space_id) = params.get("spaceId").and_then(|v| v.as_str())
            && let Some(owner) = ownership.spaces.get(space_id)
        {
            return owner.engine.clone();
        }
        if let Some(device_id) = params.get("deviceId").and_then(|v| v.as_str()) {
            if let Some(engine) = self.remotes.read().unwrap().get(device_id).cloned() {
                return engine;
            }
            if let Some(owner) = ownership.devices.get(device_id) {
                return owner.engine.clone();
            }
        }
        self.home.clone()
    }

    fn record_watch(
        &self,
        method: &str,
        source: &str,
        engine: &EngineHandle,
        rows: &[serde_json::Value],
    ) -> Vec<serde_json::Value> {
        let mut ownership = self.ownership.write().unwrap();
        let owner_map = match method {
            methods::WATCH_CHATS => &mut ownership.chats,
            methods::WATCH_SPACES => &mut ownership.spaces,
            methods::WATCH_DEVICES => &mut ownership.devices,
            // Session rows are keyed by chat but are a status projection, not
            // an ownership source. The chat watch owns this map; allowing a
            // session frame to clear it would make routing order-dependent.
            methods::WATCH_SESSIONS => return rows.to_vec(),
            _ => return rows.to_vec(),
        };

        owner_map.retain(|_, owner| owner.source != source);
        for row in rows {
            let id = row.get("id").and_then(|value| value.as_str());
            let Some(id) = id else { continue };

            let device_id = row.get("deviceId").and_then(|value| value.as_str());
            let owner_engine = if let Some(device_id) = device_id {
                self.remotes
                    .read()
                    .unwrap()
                    .get(device_id)
                    .cloned()
                    .unwrap_or_else(|| self.home.clone())
            } else {
                engine.clone()
            };
            owner_map.insert(
                id.to_string(),
                Owner {
                    source: source.to_string(),
                    engine: owner_engine,
                },
            );
        }
        drop(ownership);
        rows.to_vec()
    }

    async fn merged_watch(
        self: &Arc<Self>,
        method: &'static str,
    ) -> Result<BoxStream<'static, serde_json::Value>, RpcError> {
        let engines = self.engines().await;
        let mut inputs = futures::stream::SelectAll::new();
        let mut subscribed = 0;
        for (source, engine) in engines {
            match engine
                .client()
                .subscribe(method, serde_json::json!({}))
                .await
            {
                Ok(rx) => {
                    subscribed += 1;
                    let stream = futures::stream::unfold(rx, |mut rx| async move {
                        rx.recv().await.map(|value| (value, rx))
                    })
                    .map(move |value| (source.clone(), engine.clone(), value));
                    inputs.push(Box::pin(stream));
                }
                Err(error) => {
                    tracing::debug!(method, source, %error, "hub watch unavailable on engine");
                }
            }
        }
        if subscribed == 0 {
            return Err(RpcError::Failed(format!("no engine serves {method}")));
        }

        let hub = Arc::clone(self);
        let stream = futures::stream::unfold(
            (
                inputs,
                HashMap::<String, HashMap<String, serde_json::Value>>::new(),
                subscribed,
            ),
            move |(mut inputs, mut snapshots, expected)| {
                let hub = Arc::clone(&hub);
                async move {
                    loop {
                        let (source, engine, value) = inputs.next().await?;
                        let rows = value.as_array()?.clone();
                        let current = snapshots.entry(source.clone()).or_default();
                        current.clear();
                        for row in rows {
                            let Some(id) = row
                                .get(if method == methods::WATCH_SESSIONS {
                                    "chatId"
                                } else {
                                    "id"
                                })
                                .and_then(|value| value.as_str())
                            else {
                                continue;
                            };
                            current.insert(id.to_string(), row);
                        }

                        let owned_rows: Vec<_> = current.values().cloned().collect();
                        hub.record_watch(method, &source, &engine, &owned_rows);

                        // Do not expose a partial opening snapshot. Every
                        // engine emits its current value first, so the first
                        // router frame is already the complete union.
                        if snapshots.len() < expected {
                            continue;
                        }

                        let mut union = BTreeMap::new();
                        for source_rows in snapshots.values() {
                            for (id, row) in source_rows {
                                union.insert(id.clone(), row.clone());
                            }
                        }
                        break Some((
                            serde_json::Value::Array(union.into_values().collect()),
                            (inputs, snapshots, expected),
                        ));
                    }
                }
            },
        )
        .boxed();
        Ok(stream)
    }

    pub async fn shutdown(&self) {
        if self.shutdown.swap(true, Ordering::AcqRel) {
            return;
        }
        let remotes = {
            let mut remotes = self.remotes.write().unwrap();
            std::mem::take(&mut *remotes)
        };
        for (_, engine) in remotes {
            engine.shutdown().await;
        }
        self.home.shutdown().await;
    }
}

struct RouterService {
    hub: Weak<EngineHub>,
}

#[async_trait]
impl RpcService for RouterService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        let hub = self.hub.upgrade().ok_or(RpcError::Closed)?;
        let merged = matches!(
            method,
            methods::WATCH_SPACES
                | methods::WATCH_CHATS
                | methods::WATCH_DEVICES
                | methods::WATCH_SESSIONS
        );
        if merged {
            let method = match method {
                methods::WATCH_SPACES => methods::WATCH_SPACES,
                methods::WATCH_CHATS => methods::WATCH_CHATS,
                methods::WATCH_DEVICES => methods::WATCH_DEVICES,
                methods::WATCH_SESSIONS => methods::WATCH_SESSIONS,
                _ => unreachable!(),
            };
            return hub.merged_watch(method).await.map(RpcReply::Stream);
        }

        let engine = hub.route(&params).await;
        if zeron_engine::rpc::is_stream_method(method) {
            let rx = engine.client().subscribe(method, params).await?;
            let stream = futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|value| (value, rx))
            })
            .boxed();
            Ok(RpcReply::Stream(stream))
        } else {
            engine
                .client()
                .call(method, params)
                .await
                .map(RpcReply::Value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zeron_rpc::{RpcReply, RpcService};

    struct EchoService(&'static str);

    #[async_trait]
    impl RpcService for EchoService {
        async fn handle(
            &self,
            _method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            Ok(RpcReply::value(&json!({
                "source": self.0,
                "params": params,
            }))?)
        }
    }

    struct SnapshotService {
        devices: Vec<serde_json::Value>,
    }

    #[async_trait]
    impl RpcService for SnapshotService {
        async fn handle(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            if method == methods::WATCH_DEVICES {
                let devices = self.devices.clone();
                return Ok(RpcReply::Stream(
                    futures::stream::once(async move { serde_json::Value::Array(devices) }).boxed(),
                ));
            }
            Err(RpcError::UnknownMethod(method.into()))
        }
    }

    #[tokio::test]
    async fn explicit_target_routes_connected_device_and_unknown_target_uses_home() {
        let home = EngineHandle::from_test_client_with_info(
            memory_client(Arc::new(EchoService("home"))),
            "home",
        );
        let remote = EngineHandle::from_test_client_with_info(
            memory_client(Arc::new(EchoService("remote"))),
            "remote",
        );
        let hub = EngineHub::new(home);
        hub.add_remote(remote);
        let client = EngineHub::router_client(&hub);

        let connected = client
            .call("Echo", json!({"targetDeviceId": "remote"}))
            .await
            .unwrap();
        assert_eq!(connected["source"], "remote");

        let fallback = client
            .call("Echo", json!({"targetDeviceId": "date-synced"}))
            .await
            .unwrap();
        assert_eq!(fallback["source"], "home");
    }

    #[tokio::test]
    async fn workspace_watch_union_contains_each_engine_once() {
        let home = EngineHandle::from_test_client_with_info(
            memory_client(Arc::new(SnapshotService {
                devices: vec![json!({"id": "home-device"})],
            })),
            "home",
        );
        let remote = EngineHandle::from_test_client_with_info(
            memory_client(Arc::new(SnapshotService {
                devices: vec![json!({"id": "remote-device"})],
            })),
            "remote",
        );
        let hub = EngineHub::new(home);
        hub.add_remote(remote);
        let client = EngineHub::router_client(&hub);

        let mut stream = client
            .subscribe(methods::WATCH_DEVICES, json!({}))
            .await
            .unwrap();
        let first = stream.recv().await.unwrap();
        let mut ids: Vec<_> = first
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["home-device", "remote-device"]);
    }
}
