use crate::test_helpers::heavy_test;
use actix::{Actor, System};
use futures::{future, FutureExt};
use near_actix_test_utils::run_actix;
use near_chain_configs::ExternalStorageLocation::Filesystem;
use near_chain_configs::{DumpConfig, ExternalStorageConfig, Genesis, SyncConfig};
use near_client::GetBlock;
use near_network::tcp;
use near_network::test_utils::{convert_boot_nodes, wait_or_timeout, WaitOrTimeoutActor};
use near_o11y::testonly::init_integration_logger;
use near_o11y::WithSpanContextExt;
use nearcore::{config::GenesisExt, load_test_config, start_with_config};
use std::ops::ControlFlow;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// One client is in front, another must sync to it using state (fast) sync.
#[test]
#[cfg_attr(not(feature = "expensive_tests"), ignore)]
fn sync_state_nodes() {
    heavy_test(|| {
        init_integration_logger();

        let genesis = Genesis::test(vec!["test1".parse().unwrap()], 1);

        let (port1, port2) =
            (tcp::ListenerAddr::reserve_for_test(), tcp::ListenerAddr::reserve_for_test());
        let mut near1 = load_test_config("test1", port1, genesis.clone());
        near1.network_config.peer_store.boot_nodes = convert_boot_nodes(vec![]);
        near1.client_config.min_num_peers = 0;
        near1.client_config.epoch_sync_enabled = false;
        run_actix(async move {
            let dir1 = tempfile::Builder::new().prefix("sync_nodes_1").tempdir().unwrap();
            let nearcore::NearNode { view_client: view_client1, .. } =
                start_with_config(dir1.path(), near1).expect("start_with_config");

            let view_client2_holder = Arc::new(RwLock::new(None));
            let arbiters_holder = Arc::new(RwLock::new(vec![]));
            let arbiters_holder2 = arbiters_holder;

            WaitOrTimeoutActor::new(
                Box::new(move |_ctx| {
                    if view_client2_holder.read().unwrap().is_none() {
                        let view_client2_holder2 = view_client2_holder.clone();
                        let arbiters_holder2 = arbiters_holder2.clone();
                        let genesis2 = genesis.clone();

                        let actor = view_client1.send(GetBlock::latest().with_span_context());
                        let actor = actor.then(move |res| {
                            match &res {
                                Ok(Ok(b)) if b.header.height >= 101 => {
                                    let mut view_client2_holder2 =
                                        view_client2_holder2.write().unwrap();
                                    let mut arbiters_holder2 = arbiters_holder2.write().unwrap();

                                    if view_client2_holder2.is_none() {
                                        let mut near2 =
                                            load_test_config("test2", port2, genesis2.clone());
                                        near2.client_config.skip_sync_wait = false;
                                        near2.client_config.min_num_peers = 1;
                                        near2.network_config.peer_store.boot_nodes =
                                            convert_boot_nodes(vec![("test1", *port1)]);
                                        near2.client_config.epoch_sync_enabled = false;

                                        let dir2 = tempfile::Builder::new()
                                            .prefix("sync_nodes_2")
                                            .tempdir()
                                            .unwrap();
                                        let nearcore::NearNode {
                                            view_client: view_client2,
                                            arbiters,
                                            ..
                                        } = start_with_config(dir2.path(), near2)
                                            .expect("start_with_config");
                                        *view_client2_holder2 = Some(view_client2);
                                        *arbiters_holder2 = arbiters;
                                    }
                                }
                                Ok(Ok(b)) if b.header.height < 101 => {
                                    println!("FIRST STAGE {}", b.header.height)
                                }
                                Err(_) => return future::ready(()),
                                _ => {}
                            };
                            future::ready(())
                        });
                        actix::spawn(actor);
                    }

                    if let Some(view_client2) = &*view_client2_holder.write().unwrap() {
                        let actor = view_client2.send(GetBlock::latest().with_span_context());
                        let actor = actor.then(|res| {
                            match &res {
                                Ok(Ok(b)) if b.header.height >= 101 => System::current().stop(),
                                Ok(Ok(b)) if b.header.height < 101 => {
                                    println!("SECOND STAGE {}", b.header.height)
                                }
                                Err(_) => return future::ready(()),
                                _ => {}
                            };
                            future::ready(())
                        });
                        actix::spawn(actor);
                    } else {
                    }
                }),
                100,
                60000,
            )
            .start();
        });
    });
}

/// One client is in front, another must sync to it using state (fast) sync.
#[cfg(feature = "expensive_tests")]
#[test]
#[cfg_attr(not(feature = "expensive_tests"), ignore)]
fn sync_state_nodes_multishard() {
    heavy_test(|| {
        init_integration_logger();

        let mut genesis = Genesis::test_sharded_new_version(
            vec![
                "test1".parse().unwrap(),
                "test2".parse().unwrap(),
                "test3".parse().unwrap(),
                "test4".parse().unwrap(),
            ],
            4,
            vec![2, 2],
        );
        genesis.config.epoch_length = 150; // so that by the time test2 joins it is not kicked out yet

        run_actix(async move {
            let (port1, port2, port3, port4) = (
                tcp::ListenerAddr::reserve_for_test(),
                tcp::ListenerAddr::reserve_for_test(),
                tcp::ListenerAddr::reserve_for_test(),
                tcp::ListenerAddr::reserve_for_test(),
            );

            let mut near1 = load_test_config("test1", port1, genesis.clone());
            near1.network_config.peer_store.boot_nodes =
                convert_boot_nodes(vec![("test3", *port3), ("test4", *port4)]);
            near1.client_config.min_num_peers = 2;
            near1.client_config.min_block_production_delay = Duration::from_millis(200);
            near1.client_config.max_block_production_delay = Duration::from_millis(400);
            near1.client_config.epoch_sync_enabled = false;

            let mut near3 = load_test_config("test3", port3, genesis.clone());
            near3.network_config.peer_store.boot_nodes =
                convert_boot_nodes(vec![("test1", *port1), ("test4", *port4)]);
            near3.client_config.min_num_peers = 2;
            near3.client_config.min_block_production_delay =
                near1.client_config.min_block_production_delay;
            near3.client_config.max_block_production_delay =
                near1.client_config.max_block_production_delay;
            near3.client_config.epoch_sync_enabled = false;

            let mut near4 = load_test_config("test4", port4, genesis.clone());
            near4.network_config.peer_store.boot_nodes =
                convert_boot_nodes(vec![("test1", *port1), ("test3", *port3)]);
            near4.client_config.min_num_peers = 2;
            near4.client_config.min_block_production_delay =
                near1.client_config.min_block_production_delay;
            near4.client_config.max_block_production_delay =
                near1.client_config.max_block_production_delay;
            near4.client_config.epoch_sync_enabled = false;

            let dir1 = tempfile::Builder::new().prefix("sync_nodes_1").tempdir().unwrap();
            let nearcore::NearNode { view_client: view_client1, .. } =
                start_with_config(dir1.path(), near1).expect("start_with_config");

            let dir3 = tempfile::Builder::new().prefix("sync_nodes_3").tempdir().unwrap();
            start_with_config(dir3.path(), near3).expect("start_with_config");

            let dir4 = tempfile::Builder::new().prefix("sync_nodes_4").tempdir().unwrap();
            start_with_config(dir4.path(), near4).expect("start_with_config");

            let view_client2_holder = Arc::new(RwLock::new(None));
            let arbiter_holder = Arc::new(RwLock::new(vec![]));
            let arbiter_holder2 = arbiter_holder;

            WaitOrTimeoutActor::new(
                Box::new(move |_ctx| {
                    if view_client2_holder.read().unwrap().is_none() {
                        let view_client2_holder2 = view_client2_holder.clone();
                        let arbiter_holder2 = arbiter_holder2.clone();
                        let genesis2 = genesis.clone();

                        let actor = view_client1.send(GetBlock::latest().with_span_context());
                        let actor = actor.then(move |res| {
                            match &res {
                                Ok(Ok(b)) if b.header.height >= 101 => {
                                    let mut view_client2_holder2 =
                                        view_client2_holder2.write().unwrap();
                                    let mut arbiter_holder2 = arbiter_holder2.write().unwrap();

                                    if view_client2_holder2.is_none() {
                                        let mut near2 = load_test_config("test2", port2, genesis2);
                                        near2.client_config.skip_sync_wait = false;
                                        near2.client_config.min_num_peers = 3;
                                        near2.client_config.min_block_production_delay =
                                            Duration::from_millis(200);
                                        near2.client_config.max_block_production_delay =
                                            Duration::from_millis(400);
                                        near2.network_config.peer_store.boot_nodes =
                                            convert_boot_nodes(vec![
                                                ("test1", *port1),
                                                ("test3", *port3),
                                                ("test4", *port4),
                                            ]);
                                        near2.client_config.epoch_sync_enabled = false;

                                        let dir2 = tempfile::Builder::new()
                                            .prefix("sync_nodes_2")
                                            .tempdir()
                                            .unwrap();
                                        let nearcore::NearNode {
                                            view_client: view_client2,
                                            arbiters,
                                            ..
                                        } = start_with_config(dir2.path(), near2)
                                            .expect("start_with_config");
                                        *view_client2_holder2 = Some(view_client2);
                                        *arbiter_holder2 = arbiters;
                                    }
                                }
                                Ok(Ok(b)) if b.header.height < 101 => {
                                    println!("FIRST STAGE {}", b.header.height)
                                }
                                Err(_) => return future::ready(()),
                                _ => {}
                            };
                            future::ready(())
                        });
                        actix::spawn(actor);
                    }

                    if let Some(view_client2) = &*view_client2_holder.write().unwrap() {
                        let actor = view_client2.send(GetBlock::latest().with_span_context());
                        let actor = actor.then(|res| {
                            match &res {
                                Ok(Ok(b)) if b.header.height >= 101 => System::current().stop(),
                                Ok(Ok(b)) if b.header.height < 101 => {
                                    println!("SECOND STAGE {}", b.header.height)
                                }
                                Ok(Err(e)) => {
                                    println!("SECOND STAGE ERROR1: {:?}", e);
                                    return future::ready(());
                                }
                                Err(e) => {
                                    println!("SECOND STAGE ERROR2: {:?}", e);
                                    return future::ready(());
                                }
                                _ => {
                                    assert!(false);
                                }
                            };
                            future::ready(())
                        });
                        actix::spawn(actor);
                    }
                }),
                100,
                600000,
            )
            .start();
        });
    });
}

/// Start a validator that validators four shards. Since we only have 3 accounts one shard must have
/// empty state. Start another node that does state sync. Check state sync on empty state works.
#[test]
#[cfg_attr(not(feature = "expensive_tests"), ignore)]
fn sync_empty_state() {
    heavy_test(|| {
        init_integration_logger();

        let mut genesis = Genesis::test_sharded_new_version(
            vec!["test1".parse().unwrap(), "test2".parse().unwrap()],
            1,
            vec![1, 1, 1, 1],
        );
        genesis.config.epoch_length = 20;

        run_actix(async move {
            let (port1, port2) =
                (tcp::ListenerAddr::reserve_for_test(), tcp::ListenerAddr::reserve_for_test());
            // State sync triggers when header head is two epochs in the future.
            // Produce more blocks to make sure that state sync gets triggered when the second node starts.
            let state_sync_horizon = 10;
            let block_header_fetch_horizon = 1;
            let block_fetch_horizon = 1;

            let mut near1 = load_test_config("test1", port1, genesis.clone());
            near1.client_config.min_num_peers = 0;
            near1.client_config.min_block_production_delay = Duration::from_millis(200);
            near1.client_config.max_block_production_delay = Duration::from_millis(400);
            near1.client_config.epoch_sync_enabled = false;

            let dir1 = tempfile::Builder::new().prefix("sync_nodes_1").tempdir().unwrap();
            let nearcore::NearNode { view_client: view_client1, .. } =
                start_with_config(dir1.path(), near1).expect("start_with_config");
            let dir2 = Arc::new(tempfile::Builder::new().prefix("sync_nodes_2").tempdir().unwrap());

            let view_client2_holder = Arc::new(RwLock::new(None));
            let arbiters_holder = Arc::new(RwLock::new(vec![]));
            let arbiters_holder2 = arbiters_holder;

            WaitOrTimeoutActor::new(
                Box::new(move |_ctx| {
                    if view_client2_holder.read().unwrap().is_none() {
                        let view_client2_holder2 = view_client2_holder.clone();
                        let arbiters_holder2 = arbiters_holder2.clone();
                        let genesis2 = genesis.clone();
                        let dir2 = dir2.clone();

                        let actor = view_client1.send(GetBlock::latest().with_span_context());
                        let actor = actor.then(move |res| {
                            match &res {
                                Ok(Ok(b)) if b.header.height >= state_sync_horizon + 1 => {
                                    let mut view_client2_holder2 =
                                        view_client2_holder2.write().unwrap();
                                    let mut arbiters_holder2 = arbiters_holder2.write().unwrap();

                                    if view_client2_holder2.is_none() {
                                        let mut near2 = load_test_config("test2", port2, genesis2);
                                        near2.network_config.peer_store.boot_nodes =
                                            convert_boot_nodes(vec![("test1", *port1)]);
                                        near2.client_config.min_num_peers = 1;
                                        near2.client_config.min_block_production_delay =
                                            Duration::from_millis(200);
                                        near2.client_config.max_block_production_delay =
                                            Duration::from_millis(400);
                                        near2.client_config.state_fetch_horizon =
                                            state_sync_horizon;
                                        near2.client_config.block_header_fetch_horizon =
                                            block_header_fetch_horizon;
                                        near2.client_config.block_fetch_horizon =
                                            block_fetch_horizon;
                                        near2.client_config.tracked_shards = vec![0, 1, 2, 3];
                                        near2.client_config.epoch_sync_enabled = false;

                                        let nearcore::NearNode {
                                            view_client: view_client2,
                                            arbiters,
                                            ..
                                        } = start_with_config(dir2.path(), near2)
                                            .expect("start_with_config");
                                        *view_client2_holder2 = Some(view_client2);
                                        *arbiters_holder2 = arbiters;
                                    }
                                }
                                Ok(Ok(b)) if b.header.height <= state_sync_horizon => {
                                    println!("FIRST STAGE {}", b.header.height)
                                }
                                Err(_) => return future::ready(()),
                                _ => {}
                            };
                            future::ready(())
                        });
                        actix::spawn(actor);
                    }

                    if let Some(view_client2) = &*view_client2_holder.write().unwrap() {
                        let actor = view_client2.send(GetBlock::latest().with_span_context());
                        let actor = actor.then(|res| {
                            match &res {
                                Ok(Ok(b)) if b.header.height >= 40 => System::current().stop(),
                                Ok(Ok(b)) if b.header.height < 40 => {
                                    println!("SECOND STAGE {}", b.header.height)
                                }
                                Ok(Err(e)) => {
                                    println!("SECOND STAGE ERROR1: {:?}", e);
                                    return future::ready(());
                                }
                                Err(e) => {
                                    println!("SECOND STAGE ERROR2: {:?}", e);
                                    return future::ready(());
                                }
                                _ => {
                                    assert!(false);
                                }
                            };
                            future::ready(())
                        });
                        actix::spawn(actor);
                    }
                }),
                100,
                600000,
            )
            .start();
        });
    });
}

/// Runs one node for some time, which dumps state to a temp directory.
/// Start the second node which gets state parts from that temp directory.
#[test]
#[cfg_attr(not(feature = "expensive_tests"), ignore)]
fn sync_state_dump() {
    heavy_test(|| {
        init_integration_logger();

        let mut genesis = Genesis::test_sharded_new_version(
            vec!["test1".parse().unwrap(), "test2".parse().unwrap()],
            1,
            vec![1, 1, 1, 1],
        );
        // Needs to be long enough to give enough time to the second node to
        // start, sync headers and find a dump of state.
        genesis.config.epoch_length = 30;

        run_actix(async move {
            let (port1, port2) =
                (tcp::ListenerAddr::reserve_for_test(), tcp::ListenerAddr::reserve_for_test());
            // Produce more blocks to make sure that state sync gets triggered when the second node starts.
            let state_sync_horizon = 50;
            let block_header_fetch_horizon = 1;
            let block_fetch_horizon = 1;

            let mut near1 = load_test_config("test1", port1, genesis.clone());
            near1.client_config.min_num_peers = 0;
            // An epoch passes in about 9 seconds.
            near1.client_config.min_block_production_delay = Duration::from_millis(300);
            near1.client_config.max_block_production_delay = Duration::from_millis(600);
            near1.client_config.epoch_sync_enabled = false;
            let dump_dir = tempfile::Builder::new().prefix("state_dump_1").tempdir().unwrap();
            near1.client_config.state_sync.dump = Some(DumpConfig {
                location: Filesystem { root_dir: dump_dir.path().to_path_buf() },
                restart_dump_for_shards: None,
                iteration_delay: Some(Duration::from_millis(100)),
            });

            let dir1 = tempfile::Builder::new().prefix("sync_nodes_1").tempdir().unwrap();
            let nearcore::NearNode {
                view_client: view_client1,
                state_sync_dump_handle: _state_sync_dump_handle,
                ..
            } = start_with_config(dir1.path(), near1).expect("start_with_config");
            let dir2 = tempfile::Builder::new().prefix("sync_nodes_2").tempdir().unwrap();

            let view_client2_holder = Arc::new(RwLock::new(None));
            let arbiters_holder = Arc::new(RwLock::new(vec![]));
            let arbiters_holder2 = arbiters_holder;

            wait_or_timeout(100, 60000, || async {
                if view_client2_holder.read().unwrap().is_none() {
                    let view_client2_holder2 = view_client2_holder.clone();
                    let arbiters_holder2 = arbiters_holder2.clone();
                    let genesis2 = genesis.clone();

                    match view_client1.send(GetBlock::latest().with_span_context()).await {
                        Ok(Ok(b)) if b.header.height >= genesis.config.epoch_length + 2 => {
                            let mut view_client2_holder2 = view_client2_holder2.write().unwrap();
                            let mut arbiters_holder2 = arbiters_holder2.write().unwrap();

                            if view_client2_holder2.is_none() {
                                let mut near2 = load_test_config("test2", port2, genesis2);
                                near2.network_config.peer_store.boot_nodes =
                                    convert_boot_nodes(vec![("test1", *port1)]);
                                near2.client_config.min_num_peers = 1;
                                near2.client_config.min_block_production_delay =
                                    Duration::from_millis(300);
                                near2.client_config.max_block_production_delay =
                                    Duration::from_millis(600);
                                near2.client_config.state_fetch_horizon = state_sync_horizon;
                                near2.client_config.block_header_fetch_horizon =
                                    block_header_fetch_horizon;
                                near2.client_config.block_fetch_horizon = block_fetch_horizon;
                                near2.client_config.tracked_shards = vec![0, 1, 2, 3];
                                near2.client_config.epoch_sync_enabled = false;
                                near2.client_config.state_sync_enabled = true;
                                near2.client_config.state_sync_timeout = Duration::from_secs(1);
                                near2.client_config.state_sync.sync =
                                    SyncConfig::ExternalStorage(ExternalStorageConfig {
                                        location: Filesystem {
                                            root_dir: dump_dir.path().to_path_buf(),
                                        },
                                        num_concurrent_requests: 10,
                                    });

                                let nearcore::NearNode {
                                    view_client: view_client2, arbiters, ..
                                } = start_with_config(dir2.path(), near2)
                                    .expect("start_with_config");
                                *view_client2_holder2 = Some(view_client2);
                                *arbiters_holder2 = arbiters;
                            }
                        }
                        Ok(Ok(b)) if b.header.height <= state_sync_horizon => {
                            tracing::info!("FIRST STAGE {}", b.header.height);
                        }
                        Err(_) => {}
                        _ => {}
                    };
                    return ControlFlow::Continue(());
                }

                if let Some(view_client2) = &*view_client2_holder.write().unwrap() {
                    match view_client2.send(GetBlock::latest().with_span_context()).await {
                        Ok(Ok(b)) if b.header.height >= 40 => {
                            return ControlFlow::Break(());
                        }
                        Ok(Ok(b)) if b.header.height < 40 => {
                            tracing::info!("SECOND STAGE {}", b.header.height)
                        }
                        Ok(Err(e)) => {
                            tracing::info!("SECOND STAGE ERROR1: {:?}", e);
                        }
                        Err(e) => {
                            tracing::info!("SECOND STAGE ERROR2: {:?}", e);
                        }
                        _ => {
                            assert!(false);
                        }
                    };
                    return ControlFlow::Continue(());
                }

                panic!("Unexpected");
            })
            .await
            .unwrap();
            System::current().stop();
        });
    });
}
