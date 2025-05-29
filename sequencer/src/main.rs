use clap::Parser;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use runtime::types::{
    BID_COLLECTION_PERIOD_SECS, COMMITMENT_DURATION_SECS, ChainEvent, Commitment,
    DEFAULT_BID_SUBMISSION_PORT, DEFAULT_FULFILLMENT_PORT, DEFAULT_ORDER_BROADCAST_PORT,
    DEFAULT_SEQUENCER_HOST, GRACE_PERIOD_SECS, ORDER_BATCH_SIZE, Order, OrderId,
    SequencerToSolverMessage, SolverToSequencerMessage, current_timestamp_secs,
};
use runtime::{AppState, tcp_utils};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(long, default_value_t = 2)]
    tick: u64,
    #[clap(long, default_value_t = DEFAULT_ORDER_BROADCAST_PORT)]
    order_port: u16,
    #[clap(long, default_value_t = DEFAULT_BID_SUBMISSION_PORT)]
    bid_port: u16,
    #[clap(long, default_value_t = DEFAULT_FULFILLMENT_PORT)]
    fulfill_port: u16,
}

// store writer halves for connected solvers to broadcast messages
type SolverConnections = Arc<Mutex<HashMap<std::net::SocketAddr, tokio::net::tcp::OwnedWriteHalf>>>;

async fn handle_new_solver_connection(
    stream: TcpStream,
    addr: std::net::SocketAddr,
    solver_conns: SolverConnections,
) {
    println!("[Sequencer] New solver connected: {}", addr);
    let (reader, writer) = stream.into_split();
    solver_conns.lock().unwrap().insert(addr, writer);

    let mut buf_reader = BufReader::new(reader);
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => {
                // connection closed
                println!("[Sequencer] Solver {} disconnected.", addr);
                break;
            }
            Ok(_) => {
                // if solvers were to send messages on this main channel:
                // println!("[Sequencer] Received from {}: {}", addr, line.trim());
            }
            Err(e) => {
                eprintln!("[Sequencer] Error reading from solver {}: {}", addr, e);
                break;
            }
        }
    }
    solver_conns.lock().unwrap().remove(&addr);
    println!("[Sequencer] Cleaned up connection for solver {}", addr);
}

async fn broadcast_message_to_solvers(
    solver_conns: &SolverConnections,
    message: &SequencerToSolverMessage,
) {
    // step 1: collect the necessary information (addresses) under a short-lived lock.
    // we cannot hold OwnedWriteHalf across .await if it's taken by &mut from the MutexGuard.
    // so, we'll collect addresses and re-acquire the lock for each send, or redesign
    // how streams are managed (e.g., each stream has its own sender task via mpsc channel).

    // for this fix, let's try to send one by one, re-acquiring lock for removal.
    // a more robust solution might involve each client connection having its own task and a channel.

    let addrs_to_try: Vec<std::net::SocketAddr> = {
        let conns_guard = solver_conns.lock().unwrap();
        conns_guard.keys().cloned().collect()
    };

    let mut disconnected_addrs = Vec::new();

    for addr in addrs_to_try {
        let mut stream_maybe: Option<tokio::net::tcp::OwnedWriteHalf> = None;

        // Temporarily take the stream out or clone it if it were cloneable
        // OwnedWriteHalf is not Clone. So we must lock, get_mut, send, and drop lock quickly for each.
        // This is inefficient. A better way is to manage senders per connection.
        // However, to fix the immediate Send error with minimal change:
        // We can't hold the guard from get_mut across .await.
        // The previous fix attempt for this function was still problematic.

        // Let's try a different approach for broadcast:
        // 1. Lock, get all addresses.
        // 2. For each address, try to lock again, get the stream, send. This is bad for contention.

        // The most direct fix for the Send error with the current structure,
        // though it has performance implications with frequent lock/unlock:
        // We need to avoid holding the lock on `solver_conns` when `send_json_message` is awaited.

        // Option A: Temporarily remove the stream, send, then put it back if successful.
        let mut temp_stream: Option<tokio::net::tcp::OwnedWriteHalf> = {
            let mut conns_guard = solver_conns.lock().unwrap();
            conns_guard.remove(&addr) // Temporarily take ownership
        };

        if let Some(mut stream_to_send) = temp_stream.as_mut() {
            if let Err(e) = tcp_utils::send_json_message(&mut stream_to_send, message).await {
                eprintln!(
                    "[Sequencer] Failed to send message to solver {}: {}. Marking for removal.",
                    addr, e
                );
                disconnected_addrs.push(addr);
                // Do not put the stream back if send failed
            } else {
                // If send was successful, put the stream back
                // This re-acquires the lock briefly.
                let mut conns_guard = solver_conns.lock().unwrap();
                conns_guard.insert(addr, temp_stream.take().unwrap()); // Put it back
            }
        } else {
            // Stream was not in the map when we tried to remove it (e.g. another task removed it)
            // This can happen if multiple broadcasts are interleaved without proper stream management.
            // For now, we assume this means it's effectively disconnected.
            disconnected_addrs.push(addr);
        }
    }

    // Final cleanup of any streams that were marked disconnected but not re-inserted.
    // Note: disconnected_addrs primarily tracks failures. Streams that were successfully
    // sent to are put back. If a stream was successfully sent to but then its `addr`
    // is also in `disconnected_addrs` (e.g., due to concurrent modification logic elsewhere,
    // which shouldn't be the case here), it would be removed.
    // The logic above should ensure that only truly failed streams remain out of the map
    // or are added to `disconnected_addrs` for explicit removal if they couldn't be taken.

    if !disconnected_addrs.is_empty() {
        let mut conns_guard = solver_conns.lock().unwrap();
        for addr_to_remove in disconnected_addrs {
            conns_guard.remove(&addr_to_remove); // Ensure removal if it wasn't already.
            println!(
                "[Sequencer] Removed disconnected solver: {}",
                addr_to_remove
            );
        }
    }
}
async fn bid_submission_server(app_state: AppState, bid_port: u16) {
    let listener = TcpListener::bind(format!("{}:{}", DEFAULT_SEQUENCER_HOST, bid_port))
        .await
        .unwrap();
    println!("[Sequencer] Listening for bids on port {}...", bid_port);

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                println!("[Sequencer] Accepted bid connection from: {}", addr);
                let app_state_clone = app_state.clone();
                tokio::spawn(async move {
                    let (reader, _writer) = stream.into_split(); // _writer not used for one-way msg
                    let mut buf_reader = BufReader::new(reader);
                    match tcp_utils::read_json_message::<SolverToSequencerMessage>(&mut buf_reader)
                        .await
                    {
                        Ok(Some(SolverToSequencerMessage::SubmitBid(bid))) => {
                            println!("[Sequencer] Received bid: {:?} from {}", bid, addr);
                            let mut order_state =
                                app_state_clone.order_state_manager.lock().unwrap();
                            let mut chain_log = app_state_clone.chain_log.lock().unwrap();

                            // Validate bid (e.g., order exists, not expired, etc.)
                            if order_state
                                .active_orders_in_auction
                                .contains_key(&bid.order_id)
                                || order_state.active_commitments.contains_key(&bid.order_id)
                            {
                                order_state.add_bid(bid.clone());
                                chain_log.log_event(ChainEvent::BidReceived(bid));
                            } else {
                                eprintln!(
                                    "[Sequencer] Received bid for unknown or inactive order: {}",
                                    bid.order_id
                                );
                            }
                        }
                        Ok(Some(_)) => eprintln!(
                            "[Sequencer] Received unexpected message type on bid port from {}",
                            addr
                        ),
                        Ok(None) => {} // connection closed by client
                        Err(e) => eprintln!("[Sequencer] Error reading bid from {}: {}", addr, e),
                    }
                });
            }
            Err(e) => eprintln!("[Sequencer] Failed to accept bid connection: {}", e),
        }
    }
}

async fn fulfillment_server(app_state: AppState, fulfill_port: u16) {
    let listener = TcpListener::bind(format!("{}:{}", DEFAULT_SEQUENCER_HOST, fulfill_port))
        .await
        .unwrap();
    println!(
        "[Sequencer] Listening for fulfillments on port {}...",
        fulfill_port
    );

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                println!("[Sequencer] Accepted fulfillment connection from: {}", addr);
                let app_state_clone = app_state.clone();
                tokio::spawn(async move {
                    let (reader, _writer) = stream.into_split();
                    let mut buf_reader = BufReader::new(reader);
                    match tcp_utils::read_json_message::<SolverToSequencerMessage>(&mut buf_reader)
                        .await
                    {
                        Ok(Some(SolverToSequencerMessage::FulfillmentComplete {
                            order_id,
                            solver_id,
                        })) => {
                            println!(
                                "[Sequencer] Received fulfillment claim for order {} by solver {} from {}",
                                order_id, solver_id, addr
                            );
                            let mut escrow = app_state_clone.escrow.lock().unwrap();
                            escrow.record_fulfillment_claim(order_id, solver_id);
                        }
                        Ok(Some(_)) => eprintln!(
                            "[Sequencer] Received unexpected message type on fulfillment port from {}",
                            addr
                        ),
                        Ok(None) => {} // connection closed
                        Err(e) => {
                            eprintln!("[Sequencer] Error reading fulfillment from {}: {}", addr, e)
                        }
                    }
                });
            }
            Err(e) => eprintln!("[Sequencer] Failed to accept fulfillment connection: {}", e),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let app_state = AppState::new();
    let tick_duration = Duration::from_secs(args.tick);

    let solver_connections: SolverConnections = Arc::new(Mutex::new(HashMap::new()));

    // task 1: listen for new solver connections (for broadcasting orders/commitments)
    let order_listener =
        TcpListener::bind(format!("{}:{}", DEFAULT_SEQUENCER_HOST, args.order_port)).await?;
    println!(
        "[Sequencer] Listening for solvers on port {}...",
        args.order_port
    );
    let solver_conns_clone_listener = solver_connections.clone(); // Renamed for clarity
    tokio::spawn(async move {
        loop {
            match order_listener.accept().await {
                Ok((stream, addr)) => {
                    handle_new_solver_connection(stream, addr, solver_conns_clone_listener.clone())
                        .await;
                }
                Err(e) => eprintln!("[Sequencer] Failed to accept solver connection: {}", e),
            }
        }
    });

    // task 2: listen for bid submissions
    tokio::spawn(bid_submission_server(app_state.clone(), args.bid_port));

    // task 3: listen for fulfillment messages
    tokio::spawn(fulfillment_server(app_state.clone(), args.fulfill_port));

    // task 4: main sequencer loop (tick-based processing)
    let app_state_main_loop = app_state.clone();
    let solver_conns_main_loop = solver_connections.clone(); // This clone is for the main loop task
    tokio::spawn(async move {
        let mut order_id_counter = 100; // Start from a different number for clarity

        loop {
            tokio::time::sleep(tick_duration).await;
            println!("\n--- Sequencer Tick ---");
            let current_time = current_timestamp_secs();

            let mut batch_to_broadcast: Option<Vec<Order>> = None;
            let mut commitments_to_notify: Vec<Commitment> = Vec::new();
            let mut reassignments_to_notify: Vec<Commitment> = Vec::new();
            let mut expired_orders_to_notify_closure: Vec<OrderId> = Vec::new();

            // --- scope for phase 1: order generation & batching ---
            {
                let mut order_state = app_state_main_loop.order_state_manager.lock().unwrap();
                let mut chain_log = app_state_main_loop.chain_log.lock().unwrap();

                if order_id_counter % 2 == 0 {
                    let new_order_id = app_state_main_loop
                        .next_order_id
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let order = Order {
                        id: new_order_id,
                        description: format!("Sample Order #{}", new_order_id),
                        creation_time: current_time,
                    };
                    order_state.add_order(order.clone());
                    chain_log.log_event(ChainEvent::OrderAdded(order));
                    println!("[Sequencer] Added new order ID: {}", new_order_id);
                }
                order_id_counter += 1;

                let batch = order_state.create_order_batch(ORDER_BATCH_SIZE);
                if !batch.is_empty() {
                    let batch_ids: Vec<OrderId> = batch.iter().map(|o| o.id).collect();
                    println!("[Sequencer] Created batch with orders: {:?}", batch_ids);
                    chain_log.log_event(ChainEvent::BatchCreated(batch_ids.clone()));
                    batch_to_broadcast = Some(batch);
                }
            }

            // perform broadcast if a batch was created (outside the lock)
            if let Some(batch) = batch_to_broadcast {
                let message = SequencerToSolverMessage::NewOrderBatch(batch);
                broadcast_message_to_solvers(&solver_conns_main_loop, &message).await;
            }

            // --- scope for phase 2: bid selection ---
            {
                let mut order_state = app_state_main_loop.order_state_manager.lock().unwrap();
                let mut chain_log = app_state_main_loop.chain_log.lock().unwrap();

                let mut orders_to_select_bids_for = Vec::new();
                for (order_id, order_details) in order_state.active_orders_in_auction.iter() {
                    if order_details.creation_time + BID_COLLECTION_PERIOD_SECS <= current_time
                        && !order_state.active_commitments.contains_key(order_id)
                    {
                        orders_to_select_bids_for.push(*order_id);
                    }
                }

                for order_id in orders_to_select_bids_for {
                    if let Some(best_bid) = order_state.select_best_bid(&order_id) {
                        if best_bid.valid_till >= current_time {
                            println!(
                                "[Sequencer] Selecting best bid for order {}: {:?}",
                                order_id, best_bid
                            );
                            let commitment = Commitment {
                                order_id,
                                solver_id: best_bid.solver_id.clone(),
                                input_amount: best_bid.max_input,
                                min_output_amount: best_bid.min_output(),
                                deadline: std::cmp::min(
                                    current_time + COMMITMENT_DURATION_SECS,
                                    best_bid.valid_till,
                                ),
                            };

                            // update state *within* the lock
                            order_state.active_orders_in_auction.remove(&order_id);
                            order_state
                                .active_commitments
                                .insert(order_id, commitment.clone());
                            chain_log.log_event(ChainEvent::BidSelected {
                                order_id,
                                solver_id: commitment.solver_id.clone(),
                                commitment: commitment.clone(),
                            });
                            commitments_to_notify.push(commitment);
                        } else {
                            println!(
                                "[Sequencer] Best bid for order {} has expired. Bidder: {}, Valid Till: {}",
                                order_id, best_bid.solver_id, best_bid.valid_till
                            );
                        }
                    } else {
                        println!(
                            "[Sequencer] No valid bids for order {}. Marking as expired.",
                            order_id
                        );
                        order_state.active_orders_in_auction.remove(&order_id);
                        order_state.received_bids.remove(&order_id); // Clean up bids
                        chain_log.log_event(ChainEvent::OrderExpired(order_id));
                        expired_orders_to_notify_closure.push(order_id);
                    }
                }
            }

            // broadcast new commitments (outside lock)
            for commitment in commitments_to_notify {
                broadcast_message_to_solvers(
                    &solver_conns_main_loop,
                    &SequencerToSolverMessage::CommitmentNotification(commitment.clone()),
                )
                .await;
            }
            for order_id in expired_orders_to_notify_closure {
                broadcast_message_to_solvers(
                    &solver_conns_main_loop,
                    &SequencerToSolverMessage::AuctionClosedNoWinner(order_id),
                )
                .await;
            }

            // --- scope for phase 3: fulfillment check & reassignment ---
            {
                let mut order_state = app_state_main_loop.order_state_manager.lock().unwrap();
                let mut chain_log = app_state_main_loop.chain_log.lock().unwrap();
                let mut escrow = app_state_main_loop.escrow.lock().unwrap(); // Lock escrow here

                let mut fulfilled_order_ids_locally = Vec::new();
                let mut failed_commitments_to_reassign_locally = Vec::new();

                // Iterate over a clone of keys or collect items to avoid modifying while iterating if necessary
                // For active_commitments, iter() is fine as we modify different parts of order_state
                let active_commitments_clone: Vec<(OrderId, Commitment)> = order_state
                    .active_commitments
                    .iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect();

                for (order_id, commitment) in active_commitments_clone {
                    if current_time > commitment.deadline + GRACE_PERIOD_SECS {
                        if escrow.check_fulfillment(
                            order_id,
                            &commitment.solver_id,
                            commitment.deadline,
                        ) {
                            println!(
                                "[Sequencer] Order {} fulfilled by solver {}",
                                order_id, commitment.solver_id
                            );
                            chain_log.log_event(ChainEvent::OrderFulfilled {
                                order_id,
                                solver_id: commitment.solver_id.clone(),
                            });
                            escrow.clear_claim(order_id, &commitment.solver_id);
                            fulfilled_order_ids_locally.push(order_id);
                        } else {
                            println!(
                                "[Sequencer] Order {} NOT fulfilled by solver {} by deadline.",
                                order_id, commitment.solver_id
                            );
                            chain_log.log_event(ChainEvent::OrderFailedToFulfill {
                                order_id,
                                solver_id: commitment.solver_id.clone(),
                            });
                            escrow.clear_claim(order_id, &commitment.solver_id);
                            failed_commitments_to_reassign_locally
                                .push((order_id, commitment.solver_id.clone()));
                        }
                    }
                }

                for order_id in fulfilled_order_ids_locally {
                    order_state.active_commitments.remove(&order_id);
                    order_state.received_bids.remove(&order_id);
                }

                for (order_id, failed_solver_id) in failed_commitments_to_reassign_locally {
                    order_state.active_commitments.remove(&order_id); // Remove old one

                    if let Some(next_best_bid) =
                        order_state.get_next_best_bid(&order_id, &failed_solver_id)
                    {
                        if next_best_bid.valid_till >= current_time {
                            println!(
                                "[Sequencer] Reassigning order {} to solver {}",
                                order_id, next_best_bid.solver_id
                            );
                            let new_commitment = Commitment {
                                order_id,
                                solver_id: next_best_bid.solver_id.clone(),
                                input_amount: next_best_bid.max_input,
                                min_output_amount: next_best_bid.min_output(),
                                deadline: std::cmp::min(
                                    current_time + COMMITMENT_DURATION_SECS,
                                    next_best_bid.valid_till,
                                ),
                            };
                            order_state
                                .active_commitments
                                .insert(order_id, new_commitment.clone());
                            chain_log.log_event(ChainEvent::OrderReassigned {
                                order_id,
                                prev_solver_id: failed_solver_id,
                                new_solver_id: new_commitment.solver_id.clone(),
                                new_commitment: new_commitment.clone(),
                            });
                            reassignments_to_notify.push(new_commitment);
                        } else {
                            println!(
                                "[Sequencer] Next best bid for order {} from {} has expired. Order fails.",
                                order_id, next_best_bid.solver_id
                            );
                            chain_log.log_event(ChainEvent::OrderExpired(order_id));
                            order_state.received_bids.remove(&order_id);
                            // expired_orders_to_notify_closure.push(order_id); // Already handled or should be added to a fresh list for this scope
                        }
                    } else {
                        println!(
                            "[Sequencer] No other bids to reassign order {}. Order fails.",
                            order_id
                        );
                        chain_log.log_event(ChainEvent::OrderExpired(order_id));
                        order_state.received_bids.remove(&order_id);
                        // expired_orders_to_notify_closure.push(order_id);
                    }
                }
            } // order_state, chain_log, and escrow locks released

            // Broadcast reassignments (outside lock)
            for commitment in reassignments_to_notify {
                broadcast_message_to_solvers(
                    &solver_conns_main_loop,
                    &SequencerToSolverMessage::CommitmentNotification(commitment.clone()),
                )
                .await;
            }
        }
    });

    println!("[Sequencer] Initialized. Tick interval: {}s", args.tick);
    tokio::signal::ctrl_c().await?;
    println!("[Sequencer] Shutting down...");
    Ok(())
}
