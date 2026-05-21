use crate::plugins::plugin_worker::MessageToWorker;
use crate::screen::ScreenInstruction;
use crate::ClientId;
use std::collections::HashSet;

impl super::WasmBridge {
    /// Called from `remove_client`. When other clients remain, destroy the leaving client's
    /// instances outright. When this was the last client, quiesce the entries in place but
    /// keep them in `plugin_map` as a metadata-only carcass — `add_client` derives the plugin
    /// registry from `plugin_assets`, so removing the last entries would lose the plugins on
    /// reattach.
    pub(super) fn tear_down_client_instances(&mut self, client_id: ClientId) {
        if self.connected_clients.lock().unwrap().is_empty() {
            let any_intercepting = self
                .plugin_map
                .lock()
                .unwrap()
                .quiesce_client_instances(client_id);
            if any_intercepting {
                let _ = self
                    .senders
                    .send_to_screen(ScreenInstruction::ClearKeyPressesIntercepts(client_id));
            }
            self.clear_per_client_bridge_state(client_id);
            return;
        }
        self.destroy_instances_for(client_id);
    }

    /// Called from `add_client` after the new client's `start_plugin` tasks are queued. Drops
    /// every entry whose `client_id` is neither currently attached nor the incoming client —
    /// those are leftover carcasses kept alive only as a registry across the detach gap.
    pub(super) fn sweep_orphan_carcasses(&mut self, incoming_client_id: ClientId) {
        let connected: HashSet<ClientId> = self
            .connected_clients
            .lock()
            .unwrap()
            .iter()
            .copied()
            .collect();
        let carcass_cids: HashSet<ClientId> = self
            .plugin_map
            .lock()
            .unwrap()
            .all_plugin_ids()
            .into_iter()
            .map(|(_, cid)| cid)
            .filter(|cid| !connected.contains(cid) && *cid != incoming_client_id)
            .collect();

        for cid in carcass_cids {
            self.destroy_instances_for(cid);
        }
    }

    fn destroy_instances_for(&mut self, client_id: ClientId) {
        let removed = self
            .plugin_map
            .lock()
            .unwrap()
            .remove_client_instances(client_id);

        let any_was_intercepting = removed.iter().any(|(_, (running_plugin, _, _))| {
            running_plugin.lock().unwrap().intercepting_key_presses()
        });
        if any_was_intercepting {
            let _ = self
                .senders
                .send_to_screen(ScreenInstruction::ClearKeyPressesIntercepts(client_id));
        }

        for ((plugin_id, _cid), (running_plugin, subscriptions, workers)) in removed {
            for (_worker_name, worker_sender) in workers {
                let _ = worker_sender.send(MessageToWorker::Exit);
            }

            // The WASM Store can only be dropped on the thread it was created on.
            self.plugin_executor.execute_for_plugin(plugin_id, {
                move |_senders,
                      _plugin_map,
                      _connected_clients,
                      _plugin_cache,
                      _engine| {
                    let cache_dir = running_plugin
                        .lock()
                        .unwrap()
                        .store
                        .data()
                        .plugin_own_data_dir
                        .clone();
                    if let Err(e) = std::fs::remove_dir_all(&cache_dir) {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            log::error!(
                                "Failed to remove per-client plugin data dir on detach: {:?}",
                                e
                            );
                        }
                    }
                    drop(running_plugin);
                    drop(subscriptions);
                }
            });
        }

        self.clear_per_client_bridge_state(client_id);
    }

    fn clear_per_client_bridge_state(&mut self, client_id: ClientId) {
        self.keybinds.remove(&client_id);
        self.base_modes.remove(&client_id);
        for messages in self.cached_worker_messages.values_mut() {
            messages.retain(|(cid, _, _, _)| *cid != client_id);
        }
        self.cached_plugin_map.clear();
    }
}
