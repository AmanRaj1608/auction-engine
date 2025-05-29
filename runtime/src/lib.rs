pub mod types;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use types::{Bid, ChainEvent, Commitment, Order, OrderId, SolverId};

/// in-memory append-only log for the sequencer
#[derive(Debug, Clone, Default)]
pub struct ChainLog {
    pub events: Vec<ChainEvent>,
}

impl ChainLog {
    pub fn new() -> Self {
        Default::default()
    }
    pub fn log_event(&mut self, event: ChainEvent) {
        // just logging for now
        println!("[ChainEvent] {event:?}");
        self.events.push(event);
    }
}

/// manage the state of orders, bids, and commitments for the sequencer
#[derive(Debug, Clone, Default)]
pub struct OrderStateManager {
    pub pending_orders: VecDeque<Order>,
    pub active_orders_in_auction: HashMap<OrderId, Order>,
    pub received_bids: HashMap<OrderId, Vec<Bid>>,
    pub active_commitments: HashMap<OrderId, Commitment>,
}

impl OrderStateManager {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn add_order(&mut self, order: Order) {
        self.pending_orders.push_back(order);
    }

    pub fn create_order_batch(&mut self, max_batch_size: usize) -> Vec<Order> {
        let mut batch = Vec::new();
        for _ in 0..max_batch_size {
            if let Some(order) = self.pending_orders.pop_front() {
                self.active_orders_in_auction
                    .insert(order.id, order.clone());
                batch.push(order);
            } else {
                break;
            }
        }
        batch
    }

    pub fn add_bid(&mut self, bid: Bid) {
        if self.active_orders_in_auction.contains_key(&bid.order_id)
            || self.active_commitments.contains_key(&bid.order_id)
        {
            // allow bids even if committed for next rounds
            self.received_bids
                .entry(bid.order_id)
                .or_default()
                .push(bid);
        }
    }

    pub fn select_best_bid(&self, order_id: &OrderId) -> Option<Bid> {
        self.received_bids.get(order_id).and_then(|bids_for_order| {
            bids_for_order
                .iter()
                .filter(|b| b.valid_till >= types::current_timestamp_secs()) // Consider only valid bids
                .max_by(|a, b| {
                    a.min_output()
                        .cmp(&b.min_output())
                        .then_with(|| b.valid_till.cmp(&a.valid_till))
                })
                .cloned()
        })
    }

    pub fn get_next_best_bid(
        &self,
        order_id: &OrderId,
        failed_solver_id: &SolverId,
    ) -> Option<Bid> {
        self.received_bids.get(order_id).and_then(|bids_for_order| {
            bids_for_order
                .iter()
                .filter(|b| {
                    b.solver_id != *failed_solver_id
                        && b.valid_till >= types::current_timestamp_secs()
                })
                .max_by(|a, b| a.min_output().cmp(&b.min_output()))
                .cloned()
        })
    }
}

/// simplified in-memory escrow
#[derive(Debug, Clone, Default)]
pub struct InMemoryEscrow {
    fulfillment_claims: HashMap<(OrderId, SolverId), types::Timestamp>,
}

impl InMemoryEscrow {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn record_fulfillment_claim(&mut self, order_id: OrderId, solver_id: SolverId) {
        self.fulfillment_claims
            .insert((order_id, solver_id), types::current_timestamp_secs());
    }

    pub fn check_fulfillment(
        &self,
        order_id: OrderId,
        solver_id: &SolverId,
        commitment_deadline: types::Timestamp,
    ) -> bool {
        match self.fulfillment_claims.get(&(order_id, solver_id.clone())) {
            Some(&claim_time) => claim_time <= commitment_deadline,
            None => false,
        }
    }

    pub fn clear_claim(&mut self, order_id: OrderId, solver_id: &SolverId) {
        self.fulfillment_claims
            .remove(&(order_id, solver_id.clone()));
    }
}

/// shared application state for the sequencer
#[derive(Clone)]
pub struct AppState {
    pub chain_log: Arc<Mutex<ChainLog>>,
    pub order_state_manager: Arc<Mutex<OrderStateManager>>,
    pub escrow: Arc<Mutex<InMemoryEscrow>>,
    pub next_order_id: Arc<std::sync::atomic::AtomicU64>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        Self {
            chain_log: Arc::new(Mutex::new(ChainLog::new())),
            order_state_manager: Arc::new(Mutex::new(OrderStateManager::new())),
            escrow: Arc::new(Mutex::new(InMemoryEscrow::new())),
            next_order_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        }
    }
}

// helper for simple line-based json messaging over tcp
pub mod tcp_utils {
    use serde::{de::DeserializeOwned, Serialize};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

    pub async fn send_json_message<T: Serialize>(
        stream: &mut OwnedWriteHalf,
        message: &T,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut json_string = serde_json::to_string(message)?;
        json_string.push('\n');
        stream.write_all(json_string.as_bytes()).await?;
        Ok(())
    }

    pub async fn read_json_message<T: DeserializeOwned>(
        reader: &mut BufReader<OwnedReadHalf>,
    ) -> Result<Option<T>, Box<dyn std::error::Error>> {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => Ok(None),
            Ok(_) => {
                if line.trim().is_empty() {
                    return Ok(None);
                }
                match serde_json::from_str::<T>(line.trim()) {
                    Ok(msg) => Ok(Some(msg)),
                    Err(e) => {
                        eprintln!("Deserialization error: {} for line: '{}'", e, line.trim());
                        Err(Box::new(e))
                    }
                }
            }
            Err(e) => Err(Box::new(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        current_timestamp_secs, Bid, ChainEvent, Commitment, Order, COMMITMENT_DURATION_SECS,
        GRACE_PERIOD_SECS,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn test_happy_path_order_fulfillment() {
        let mut order_manager = OrderStateManager::new();
        let mut escrow = InMemoryEscrow::new();
        let mut chain_log = ChainLog::new();

        let order = Order {
            id: 1,
            description: "Test Order #1".to_string(),
            creation_time: current_timestamp_secs(),
        };

        order_manager.add_order(order.clone());
        chain_log.log_event(ChainEvent::OrderAdded(order.clone()));

        let batch = order_manager.create_order_batch(5);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, 1);

        let bid = Bid {
            order_id: 1,
            solver_id: "solver1".to_string(),
            max_input: 100,
            conversion_rate: 1.5,
            valid_till: current_timestamp_secs() + 60,
        };

        order_manager.add_bid(bid.clone());
        chain_log.log_event(ChainEvent::BidReceived(bid.clone()));

        let best_bid = order_manager.select_best_bid(&1).unwrap();
        assert_eq!(best_bid.solver_id, "solver1");
        assert_eq!(best_bid.min_output(), 150);

        let commitment = Commitment {
            order_id: 1,
            solver_id: best_bid.solver_id.clone(),
            input_amount: best_bid.max_input,
            min_output_amount: best_bid.min_output(),
            deadline: current_timestamp_secs() + COMMITMENT_DURATION_SECS,
        };

        order_manager
            .active_commitments
            .insert(1, commitment.clone());

        escrow.record_fulfillment_claim(1, "solver1".to_string());

        let is_fulfilled = escrow.check_fulfillment(1, &"solver1".to_string(), commitment.deadline);
        assert!(is_fulfilled);
    }

    #[test]
    fn test_order_reassignment_after_failed_fulfillment() {
        let mut order_manager = OrderStateManager::new();
        let mut escrow = InMemoryEscrow::new();
        let mut chain_log = ChainLog::new();

        let order = Order {
            id: 2,
            description: "Test Order #2".to_string(),
            creation_time: current_timestamp_secs(),
        };

        order_manager.add_order(order.clone());
        chain_log.log_event(ChainEvent::OrderAdded(order.clone()));

        let batch = order_manager.create_order_batch(5);
        assert_eq!(batch.len(), 1);

        let bid1 = Bid {
            order_id: 2,
            solver_id: "solver1".to_string(),
            max_input: 100,
            conversion_rate: 1.5,
            valid_till: current_timestamp_secs() + 120,
        };

        let bid2 = Bid {
            order_id: 2,
            solver_id: "solver2".to_string(),
            max_input: 100,
            conversion_rate: 1.3,
            valid_till: current_timestamp_secs() + 120,
        };

        order_manager.add_bid(bid1.clone());
        order_manager.add_bid(bid2.clone());
        chain_log.log_event(ChainEvent::BidReceived(bid1.clone()));
        chain_log.log_event(ChainEvent::BidReceived(bid2.clone()));

        let best_bid = order_manager.select_best_bid(&2).unwrap();
        assert_eq!(best_bid.solver_id, "solver1");

        let commitment1 = Commitment {
            order_id: 2,
            solver_id: best_bid.solver_id.clone(),
            input_amount: best_bid.max_input,
            min_output_amount: best_bid.min_output(),
            deadline: current_timestamp_secs() + 5,
        };

        order_manager
            .active_commitments
            .insert(2, commitment1.clone());

        std::thread::sleep(Duration::from_secs(6 + GRACE_PERIOD_SECS + 1));

        let is_fulfilled =
            escrow.check_fulfillment(2, &"solver1".to_string(), commitment1.deadline);
        assert!(!is_fulfilled);

        let next_best = order_manager
            .get_next_best_bid(&2, &"solver1".to_string())
            .unwrap();
        assert_eq!(next_best.solver_id, "solver2");
        assert_eq!(next_best.min_output(), 130);

        let commitment2 = Commitment {
            order_id: 2,
            solver_id: next_best.solver_id.clone(),
            input_amount: next_best.max_input,
            min_output_amount: next_best.min_output(),
            deadline: current_timestamp_secs() + COMMITMENT_DURATION_SECS,
        };

        order_manager.active_commitments.remove(&2);
        order_manager
            .active_commitments
            .insert(2, commitment2.clone());

        chain_log.log_event(ChainEvent::OrderReassigned {
            order_id: 2,
            prev_solver_id: "solver1".to_string(),
            new_solver_id: "solver2".to_string(),
            new_commitment: commitment2.clone(),
        });

        escrow.record_fulfillment_claim(2, "solver2".to_string());
        let is_fulfilled2 =
            escrow.check_fulfillment(2, &"solver2".to_string(), commitment2.deadline);
        assert!(is_fulfilled2);
    }

    #[test]
    fn test_no_valid_bids_order_expiry() {
        let mut order_manager = OrderStateManager::new();
        let mut chain_log = ChainLog::new();

        let order = Order {
            id: 3,
            description: "Test Order #3".to_string(),
            creation_time: current_timestamp_secs(),
        };

        order_manager.add_order(order.clone());
        let batch = order_manager.create_order_batch(5);
        assert_eq!(batch.len(), 1);

        let expired_bid = Bid {
            order_id: 3,
            solver_id: "solver1".to_string(),
            max_input: 100,
            conversion_rate: 1.5,
            valid_till: current_timestamp_secs() - 10,
        };

        order_manager.add_bid(expired_bid);

        let best_bid = order_manager.select_best_bid(&3);
        assert!(best_bid.is_none());

        chain_log.log_event(ChainEvent::OrderExpired(3));
    }

    #[test]
    fn test_concurrent_bid_handling() {
        let order_manager = Arc::new(Mutex::new(OrderStateManager::new()));

        let order = Order {
            id: 4,
            description: "Test Order #4".to_string(),
            creation_time: current_timestamp_secs(),
        };

        order_manager.lock().unwrap().add_order(order.clone());
        order_manager.lock().unwrap().create_order_batch(5);

        let handles: Vec<_> = (0..5)
            .map(|i| {
                let om = Arc::clone(&order_manager);
                std::thread::spawn(move || {
                    let bid = Bid {
                        order_id: 4,
                        solver_id: format!("solver{i}"),
                        max_input: 100 + i * 10,
                        conversion_rate: 1.0 + (i as f64 * 0.1),
                        valid_till: current_timestamp_secs() + 60,
                    };
                    om.lock().unwrap().add_bid(bid);
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        let bids_count = order_manager
            .lock()
            .unwrap()
            .received_bids
            .get(&4)
            .map(|bids| bids.len())
            .unwrap_or(0);
        assert_eq!(bids_count, 5);

        let best_bid = order_manager.lock().unwrap().select_best_bid(&4).unwrap();
        assert_eq!(best_bid.solver_id, "solver4");
        assert_eq!(best_bid.min_output(), 196);
    }
}
